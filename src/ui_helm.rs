//! Helm navigation keeps release storage identities separate from Kubernetes objects.
use crate::{
    Result,
    gateway::Gateway,
    helm::{self, Release, Snapshot, Storage},
    safety,
};
use crossterm::event::KeyCode;

#[derive(Clone)]
pub enum Request {
    List(String, Option<Storage>),
    NamedHistory(String, String, Option<Storage>),
    History(Release),
    Detail(Release),
    Resources(Release),
    Values(Release),
}
impl Request {
    pub fn label(&self) -> &'static str {
        match self {
            Self::List(..) => "Reading Helm release metadata...",
            Self::NamedHistory(..) | Self::History(_) => "Reading release revision history...",
            Self::Detail(_) => "Reading selected revision metadata...",
            Self::Resources(_) => "Reading selected revision resource identities...",
            Self::Values(_) => "Reading and redacting selected revision stored values...",
        }
    }
}
pub enum Response {
    Table(Box<Table>),
    Document(String, String),
}

pub async fn read(gateway: &Gateway, request: Request) -> Result<Response> {
    Ok(match request {
        Request::List(namespace, storage) => Response::Table(Box::new(Table::new(
            helm::list_with_storage(gateway, &namespace, storage).await?,
            None,
            namespace,
            storage,
        ))),
        Request::NamedHistory(namespace, name, storage) => {
            let latest = helm::list_with_storage(gateway, &namespace, storage).await?;
            let release = helm::select_release(&latest, &name, storage)?;
            let snapshot = helm::history(gateway, &release).await?;
            Response::Table(Box::new(Table::new(
                snapshot,
                Some(release),
                namespace,
                storage,
            )))
        }
        Request::History(release) => {
            let storage = Some(release.storage);
            let snapshot = helm::history(gateway, &release).await?;
            let namespace = release.namespace.clone();
            Response::Table(Box::new(Table::new(
                snapshot,
                Some(release),
                namespace,
                storage,
            )))
        }
        Request::Detail(release) => Response::Document(
            "HELM REVISION METADATA".into(),
            helm::detail(gateway, &release).await?.text(),
        ),
        Request::Resources(release) => {
            let detail = helm::detail(gateway, &release).await?;
            let mut lines = vec![
                format!(
                    "{}/{} | revision {} | {} storage",
                    release.namespace,
                    release.name,
                    release.revision,
                    release.storage.name()
                ),
                "Resource identities only; no rendered manifest bodies.".into(),
                String::new(),
            ];
            lines.extend(detail.resources.iter().map(|resource| {
                format!(
                    "{} {} {}/{}",
                    resource.api_version,
                    resource.kind,
                    if resource.namespace.is_empty() {
                        "[namespace omitted / cluster scoped]"
                    } else {
                        &resource.namespace
                    },
                    resource.name
                )
            }));
            if detail.resources_truncated {
                lines.push("INCOMPLETE resource references".into());
            }
            lines.extend(detail.issues);
            Response::Document("HELM RELEASE RESOURCES".into(), lines.join("\n"))
        }
        Request::Values(release) => Response::Document(
            "HELM STORED VALUES / known fields redacted".into(),
            format!(
                "{} storage\n{}",
                release.storage.name(),
                helm::values(gateway, &release).await?.text()?
            ),
        ),
    })
}

pub enum Action {
    None,
    Close,
    Request(Request),
    Metadata(Release),
    Lifecycle(Box<Intent>),
    Unavailable(&'static str),
}
#[derive(Clone)]
pub enum Intent {
    Install,
    InstallCrds,
    UpgradeCrds,
    DeleteCrds,
    MigrateCrds,
    Test(Release),
    Upgrade(Release),
    Rollback {
        current: Release,
        target: Box<Release>,
    },
    Uninstall(Release),
}
impl Intent {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Install => "INSTALL HELM RELEASE",
            Self::InstallCrds => "INSTALL CHART CRDS",
            Self::UpgradeCrds => "UPGRADE CHART CRDS",
            Self::DeleteCrds => "DELETE CHART CRDS",
            Self::MigrateCrds => "MIGRATE CRD STORAGE",
            Self::Test(_) => "TEST HELM RELEASE",
            Self::Upgrade(_) => "UPGRADE HELM RELEASE",
            Self::Rollback { .. } => "ROLL BACK HELM RELEASE",
            Self::Uninstall(_) => "UNINSTALL HELM RELEASE",
        }
    }
    pub fn is_crd(&self) -> bool {
        matches!(
            self,
            Self::InstallCrds | Self::UpgradeCrds | Self::DeleteCrds | Self::MigrateCrds
        )
    }
    fn has_chart_input(&self) -> bool {
        matches!(
            self,
            Self::Install
                | Self::Upgrade(_)
                | Self::InstallCrds
                | Self::UpgradeCrds
                | Self::DeleteCrds
                | Self::MigrateCrds
        )
    }
    fn current(&self) -> Option<&Release> {
        match self {
            Self::Install
            | Self::InstallCrds
            | Self::UpgradeCrds
            | Self::DeleteCrds
            | Self::MigrateCrds => None,
            Self::Upgrade(release) | Self::Uninstall(release) | Self::Test(release) => {
                Some(release)
            }
            Self::Rollback { current, .. } => Some(current),
        }
    }
}
pub struct Form {
    pub intent: Intent,
    pub fields: Vec<(&'static str, String)>,
    pub active: usize,
    namespace: String,
    storage: Storage,
    allow_hooks: bool,
    allow_crds: bool,
    registry: crate::helm_oci::Options,
    keyring: Option<std::path::PathBuf>,
    sigstore_policy: Option<std::path::PathBuf>,
}
pub struct WriteDraft {
    pub options: crate::helm_lifecycle::Options,
    pub request: crate::helm_lifecycle::Request,
    pub expected: Option<Release>,
    pub rollback_target: Option<Release>,
}
/// Prefilled simulated chart for demo forms. It must be absolute on the host
/// platform because `Form::draft` rejects non-absolute local chart paths.
const DEMO_CHART: &str = if cfg!(windows) {
    r"C:\demo\chart.tgz"
} else {
    "/demo/chart.tgz"
};
impl Form {
    pub fn new(
        intent: Intent,
        namespace: String,
        storage: Storage,
        values: Option<&std::path::Path>,
        demo: bool,
    ) -> Self {
        let chart = if demo {
            DEMO_CHART.into()
        } else {
            String::new()
        };
        let values = values
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fields = match &intent {
            Intent::Install
            | Intent::InstallCrds
            | Intent::UpgradeCrds
            | Intent::DeleteCrds
            | Intent::MigrateCrds => {
                vec![
                    (
                        "Namespace",
                        if namespace == "*" {
                            String::new()
                        } else {
                            namespace.clone()
                        },
                    ),
                    ("Release", String::new()),
                    ("Chart source", chart),
                ]
            }
            Intent::Upgrade(_) => vec![
                ("Chart source", chart),
                ("Values file (optional)", values.clone()),
            ],
            _ => vec![],
        };
        let mut fields = fields;
        if matches!(intent, Intent::Install) {
            fields.push(("Values file (optional)", values));
        }
        Self {
            intent,
            fields,
            active: 0,
            namespace,
            storage,
            allow_hooks: false,
            allow_crds: false,
            registry: Default::default(),
            keyring: None,
            sigstore_policy: None,
        }
    }
    pub fn set_lifecycle_policy(
        &mut self,
        allow_hooks: bool,
        allow_crds: bool,
        registry: crate::helm_oci::Options,
    ) {
        self.allow_hooks = allow_hooks;
        self.allow_crds = allow_crds;
        self.registry = registry;
    }
    pub fn set_keyring(&mut self, keyring: Option<std::path::PathBuf>) {
        self.keyring = keyring;
    }
    pub fn set_sigstore_policy(&mut self, policy: Option<std::path::PathBuf>) {
        self.sigstore_policy = policy;
    }
    pub fn key(&mut self, key: KeyCode) {
        let count = self.fields.len();
        if count == 0 {
            return;
        }
        match key {
            KeyCode::Tab | KeyCode::Down => self.active = (self.active + 1) % count,
            KeyCode::BackTab | KeyCode::Up => {
                self.active = if self.active == 0 {
                    count - 1
                } else {
                    self.active - 1
                }
            }
            KeyCode::Backspace => {
                self.fields[self.active].1.pop();
            }
            KeyCode::Char(c)
                if !c.is_control() && self.fields[self.active].1.len() + c.len_utf8() <= 4096 =>
            {
                self.fields[self.active].1.push(c)
            }
            _ => {}
        }
    }
    pub fn clear(&mut self) {
        if let Some((_, value)) = self.fields.get_mut(self.active) {
            value.clear();
        }
    }
    pub fn draft(&self, executable: std::path::PathBuf, seconds: u16) -> Result<WriteDraft> {
        use crate::helm_lifecycle::{Options, Request};
        if self.intent.is_crd() && !self.allow_crds {
            return Err("Chart CRD actions require explicit --allow-helm-crds.".into());
        }
        if matches!(self.intent, Intent::Test(_)) && !self.allow_hooks {
            return Err("Helm tests require explicit --allow-helm-hooks.".into());
        }
        let path = |value: &str, optional: bool| -> Result<Option<std::path::PathBuf>> {
            if value.is_empty() {
                return if optional {
                    Ok(None)
                } else {
                    Err("Choose an absolute local packaged chart .tgz path.".into())
                };
            }
            let path = std::path::PathBuf::from(value);
            if !path.is_absolute() {
                return Err("Chart and values paths must be absolute local paths; no shell expansion is performed.".into());
            }
            Ok(Some(path))
        };
        let chart = |value: &str| -> Result<std::path::PathBuf> {
            if value.starts_with("oci://") {
                crate::helm_oci::Reference::parse(value)?;
                Ok(value.into())
            } else {
                let path = path(value, false)?.ok_or("Missing chart source")?;
                if path.extension().is_none_or(|extension| extension != "tgz") {
                    return Err(
                        "Choose a packaged .tgz chart or an OCI reference with an explicit tag or sha256 digest."
                            .into(),
                    );
                }
                Ok(path)
            }
        };
        let (namespace, release, storage, request) = match &self.intent {
            Intent::Install
            | Intent::InstallCrds
            | Intent::UpgradeCrds
            | Intent::DeleteCrds
            | Intent::MigrateCrds => {
                let namespace = self.fields[0].1.trim();
                let release = self.fields[1].1.trim();
                if !safety::valid_id(namespace) {
                    return Err("Choose one explicit namespace before installation.".into());
                }
                if !helm::valid_release_name(release) {
                    return Err("Release name must be a DNS name of at most 53 bytes.".into());
                }
                let chart = chart(&self.fields[2].1)?;
                let request = match self.intent {
                    Intent::InstallCrds => Request::InstallCrds { chart },
                    Intent::UpgradeCrds => Request::UpgradeCrds { chart },
                    Intent::DeleteCrds => Request::DeleteCrds { chart },
                    Intent::MigrateCrds => Request::MigrateCrds { chart },
                    _ => Request::Install {
                        chart,
                        values: path(&self.fields[3].1, true)?,
                    },
                };
                (namespace.into(), release.into(), self.storage, request)
            }
            Intent::Upgrade(current) => (
                current.namespace.clone(),
                current.name.clone(),
                current.storage,
                Request::Upgrade {
                    chart: chart(&self.fields[0].1)?,
                    values: path(&self.fields[1].1, true)?,
                },
            ),
            Intent::Rollback { current, target } => (
                current.namespace.clone(),
                current.name.clone(),
                current.storage,
                Request::Rollback {
                    revision: target.revision,
                },
            ),
            Intent::Test(current) => (
                current.namespace.clone(),
                current.name.clone(),
                current.storage,
                Request::Test,
            ),
            Intent::Uninstall(current) => (
                current.namespace.clone(),
                current.name.clone(),
                current.storage,
                Request::Uninstall,
            ),
        };
        Ok(WriteDraft {
            options: Options {
                executable,
                namespace,
                release,
                storage,
                seconds,
                allow_hooks: self.allow_hooks,
                allow_crds: self.allow_crds,
                registry: self.registry.clone(),
                keyring: self.keyring.clone(),
                sigstore_policy: self.sigstore_policy.clone(),
            },
            request,
            expected: self.intent.current().cloned(),
            rollback_target: if let Intent::Rollback { target, .. } = &self.intent {
                Some((**target).clone())
            } else {
                None
            },
        })
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let roomy = height >= self.fields.len() + 10 && width >= 54;
        let target = if let Some(current) = self.intent.current() {
            format!(
                "{}/{}  |  revision {}  |  {}",
                current.namespace,
                current.name,
                current.revision,
                current.storage.name()
            )
        } else if self.intent.is_crd() {
            format!(
                "Cluster-wide schemas  |  label {}",
                self.fields
                    .first()
                    .map(|(_, value)| value.as_str())
                    .unwrap_or(&self.namespace)
            )
        } else {
            format!(
                "Namespace {}  |  {} storage",
                self.fields
                    .first()
                    .map(|(_, value)| value.as_str())
                    .unwrap_or(&self.namespace),
                self.storage.name()
            )
        };
        let mut lines = Vec::new();
        if roomy {
            lines.push("  01 CONFIGURE    /    02 Review    /    03 Execute".into());
            lines.push(String::new());
        }
        lines.push(format!("  Target  {target}"));
        if let Intent::Rollback { target, .. } = &self.intent {
            lines.push(format!(
                "  To      revision {}  |  {} {}",
                target.revision, target.chart_name, target.chart_version
            ));
        }
        if height >= self.fields.len() + 3 {
            lines.push(format!(
                "  Checks  hooks {}  /  CRDs {}  /  signature {}",
                if self.allow_hooks { "ON" } else { "OFF" },
                if self.allow_crds { "ON" } else { "OFF" },
                if !self.intent.has_chart_input() {
                    "n/a (stored release)"
                } else if self.keyring.is_some() || self.sigstore_policy.is_some() {
                    "REQUIRED"
                } else {
                    "not checked"
                }
            ));
            if self.intent.has_chart_input()
                && (self.keyring.is_some() || self.sigstore_policy.is_some())
            {
                lines.push(format!(
                    "  Trust   {} / checked during preparation",
                    match (self.keyring.is_some(), self.sigstore_policy.is_some()) {
                        (true, true) => "OpenPGP + Sigstore",
                        (true, false) => "OpenPGP public keyring",
                        _ => "Sigstore bundle + explicit trust",
                    }
                ));
            }
            if let Some((_, source)) = self
                .fields
                .iter()
                .find(|(label, _)| *label == "Chart source")
            {
                lines.push(
                    if let Ok(reference) = crate::helm_oci::Reference::parse(source) {
                        if let Some(digest) = reference.digest() {
                            let digest = digest.trim_start_matches("sha256:");
                            format!(
                                "  Source  OCI sha256:{}...{}  |  {}",
                                &digest[..12],
                                &digest[digest.len() - 8..],
                                reference.registry()
                            )
                        } else {
                            format!(
                                "  Source  OCI tag {}  |  digest resolved during preparation",
                                reference.tag().unwrap_or("?")
                            )
                        }
                    } else {
                        "  Source  Local .tgz or OCI with an explicit tag or digest".into()
                    },
                );
            }
        }
        if roomy {
            lines.push(String::new());
            lines.push("  Details".into());
        }
        let mut suffix = Vec::new();
        if roomy {
            suffix.push(String::new());
            suffix.push(if matches!(self.intent, Intent::DeleteCrds) {
                "  DESTRUCTIVE  Owned custom instances and cluster-wide schemas will be deleted."
                    .into()
            } else if matches!(self.intent, Intent::MigrateCrds) {
                "  IMPACT  Rewrite owned instances; old storage versions need controller proof."
                    .into()
            } else if matches!(self.intent, Intent::UpgradeCrds) {
                "  IMPACT  Cluster-wide schema changes; existing instances are checked.".into()
            } else if matches!(self.intent, Intent::InstallCrds) {
                "  IMPACT  Namespaced custom resources; schemas retained on uninstall.".into()
            } else if matches!(self.intent, Intent::Uninstall(_)) {
                "  DESTRUCTIVE  Release-managed resources will be removed.".into()
            } else if matches!(self.intent, Intent::Test(_)) {
                "  IMPACT  Stored test hooks execute; release revision stays unchanged.".into()
            } else {
                "  Enter prepares the review. Approval comes next.".into()
            });
        }
        if self.intent.is_crd() && height >= self.fields.len() + 4 {
            suffix.push("  Separate CRD operation; no Helm release is installed.".into());
        }
        // Reserve a visible input row even at 28x10. Metadata never hides the
        // active field, and long paths retain the editable tail.
        let capacity = height.saturating_sub(lines.len() + suffix.len()).max(1);
        lines.truncate(height.saturating_sub(1).max(1));
        let start = self.active.saturating_sub(capacity.saturating_sub(1));
        for (index, (label, value)) in self.fields.iter().enumerate().skip(start).take(capacity) {
            let label = if width < 40 {
                match *label {
                    "Chart source" => "Chart",
                    "Values file (optional)" => "Values",
                    "Namespace" => "NS",
                    label => label,
                }
            } else {
                match *label {
                    "Values file (optional)" => "Values file",
                    label => label,
                }
            };
            let prefix = if width >= 54 {
                format!(
                    " {} {:<13} ",
                    if index == self.active { ">" } else { " " },
                    label
                )
            } else {
                format!(
                    " {} {label}: ",
                    if index == self.active { ">" } else { " " }
                )
            };
            lines.push(format!(
                "{prefix}{}{}",
                tail(value, width.saturating_sub(prefix.len() + 1)),
                if index == self.active { "_" } else { "" }
            ));
        }
        if self.fields.is_empty() && height > lines.len() + suffix.len() {
            lines.push("  Ready to prepare the complete operation review.".into());
        }
        lines.extend(suffix);
        lines.truncate(height);
        lines
    }
}

pub fn same_release(a: &Release, b: &Release) -> bool {
    a.namespace == b.namespace
        && a.name == b.name
        && a.storage == b.storage
        && a.revision == b.revision
        && a.uid == b.uid
        && a.resource_version == b.resource_version
}
pub fn validate_prepared(
    prepared: &crate::helm_lifecycle::Prepared,
    draft: &WriteDraft,
) -> Result<()> {
    if draft.expected.as_ref().is_some_and(|expected| {
        prepared
            .selected()
            .is_none_or(|selected| !same_release(expected, selected))
    }) {
        return Err(
            "Selected Helm release changed before preparation; refresh and select it again.".into(),
        );
    }
    if draft.rollback_target.as_ref().is_some_and(|expected| {
        prepared
            .rollback_target()
            .is_none_or(|selected| !same_release(expected, selected))
    }) {
        return Err(
            "Selected rollback revision was replaced; refresh history before review.".into(),
        );
    }
    Ok(())
}

/// Coverage is recorded only after a frame is successfully written to the terminal.
/// Jumping to the end never marks skipped review lines as seen.
pub struct Review {
    pub prepared: Box<crate::helm_lifecycle::Prepared>,
    pub typed: String,
    pub offset: usize,
    pub epoch: u64,
    width: usize,
    text: String,
    display_valid: bool,
    viewed: std::collections::BTreeSet<usize>,
}
impl Review {
    pub fn new(prepared: Box<crate::helm_lifecycle::Prepared>, epoch: u64) -> Self {
        let display = safety::review_display(&Self::presentation(&prepared));
        Self {
            display_valid: display.is_ok(),
            text: display
                .unwrap_or_else(|error| format!("REVIEW BLOCKED\n\n{error}\n\nEsc returns.")),
            prepared,
            typed: String::new(),
            offset: 0,
            epoch,
            width: 0,
            viewed: Default::default(),
        }
    }
    fn presentation(prepared: &crate::helm_lifecycle::Prepared) -> String {
        let overview = prepared.overview();
        let mut introduction = vec![
            "01 Configure    /    02 REVIEW    /    03 Execute".into(),
            String::new(),
            format!(
                "OPERATION   {}  /  NOT EXECUTED",
                overview.operation.to_ascii_uppercase()
            ),
            format!("TARGET      {}", overview.target),
            format!("CONNECTION  {}  |  {}", overview.context, overview.endpoint),
            format!("TRUST       {}", overview.trust),
            String::new(),
            if prepared.is_demo() {
                "CHANGE PLAN  Simulated plan / no cluster writes".into()
            } else {
                format!(
                    "CHANGE PLAN  {} resources  /  {} hooks  /  {} CRDs",
                    overview.resources, overview.hooks, overview.crds
                )
            },
        ];
        if let Some(source) = overview.source {
            introduction.push(format!("SOURCE       {source}"));
        }
        if overview.destructive {
            introduction.push(
                "IMPACT       DESTRUCTIVE / schema-changing operation. Review all details.".into(),
            );
        }
        introduction.extend([
            String::new(),
            "APPROVAL     Read every line, then type the exact confirmation below.".into(),
            String::new(),
            "FULL REVIEW".into(),
        ]);
        format!("{}\n{}", introduction.join("\n"), prepared.summary())
    }
    fn wrapped(&self, width: usize) -> Vec<String> {
        super::wrap_text(&self.text, width.saturating_sub(2).max(1))
            .lines()
            .map(|line| format!("  {line}"))
            .collect()
    }
    pub fn progress(&self, width: usize) -> (usize, usize) {
        let count = self.wrapped(width).len();
        (count.saturating_sub(self.remaining(width)), count)
    }
    pub fn mark_drawn(&mut self, width: usize, height: usize) {
        if width != self.width {
            self.width = width;
            self.viewed.clear();
        }
        let count = self.wrapped(width).len();
        self.offset = self.offset.min(count.saturating_sub(1));
        self.viewed
            .extend(self.offset.min(count)..self.offset.saturating_add(height).min(count));
    }
    pub fn remaining(&self, width: usize) -> usize {
        let count = self.wrapped(width).len();
        if self.width != width {
            count
        } else {
            count.saturating_sub(self.viewed.len())
        }
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let lines = self.wrapped(width);
        let start = self.offset.min(lines.len().saturating_sub(1));
        lines.into_iter().skip(start).take(height).collect()
    }
    pub fn key(&mut self, key: KeyCode, width: usize, height: usize) {
        let count = self.wrapped(width).len();
        let page = height.saturating_sub(1).max(1);
        match key {
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::Down => self.offset = self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(page),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(page),
            KeyCode::Home => self.offset = 0,
            KeyCode::End => self.offset = count.saturating_sub(height.max(1)),
            KeyCode::Backspace => {
                self.typed.pop();
            }
            KeyCode::Char(c) if !c.is_control() && self.typed.len() + c.len_utf8() <= 320 => {
                self.typed.push(c)
            }
            _ => {}
        }
        self.offset = self.offset.min(count.saturating_sub(1));
    }
    pub fn can_execute(&self, width: u16, height: u16, epoch: u64) -> bool {
        width >= 28
            && height >= 10
            && self.display_valid
            && self.epoch == epoch
            && self.remaining(usize::from(width.saturating_sub(2))) == 0
            && self.typed == self.prepared.confirmation()
    }
}
pub struct Table {
    pub snapshot: Snapshot,
    pub parent: Option<Release>,
    pub namespace: String,
    pub storage: Option<Storage>,
    selected: usize,
    filter: String,
    draft: Option<String>,
    revision_sort: bool,
}
impl Table {
    pub fn new(
        snapshot: Snapshot,
        parent: Option<Release>,
        namespace: String,
        storage: Option<Storage>,
    ) -> Self {
        Self {
            snapshot,
            parent,
            namespace,
            storage,
            selected: 0,
            filter: String::new(),
            draft: None,
            revision_sort: true,
        }
    }
    pub fn title(&self) -> &'static str {
        if self.parent.is_some() {
            "HELM HISTORY / selected stored revisions"
        } else {
            "HELM / releases by namespace and storage"
        }
    }
    pub fn is_filtering(&self) -> bool {
        self.draft.is_some()
    }
    pub fn selected(&self) -> Option<&Release> {
        self.rows().get(self.selected).copied()
    }
    pub fn current(&self) -> Option<Release> {
        self.parent.clone().or_else(|| self.selected().cloned())
    }
    pub fn refresh(&self) -> Request {
        self.parent
            .clone()
            .map(Request::History)
            .unwrap_or_else(|| Request::List(self.namespace.clone(), self.storage))
    }
    fn rows(&self) -> Vec<&Release> {
        let filter = super::nav::Filter::parse(&self.filter).ok();
        let mut rows = self
            .snapshot
            .releases
            .iter()
            .filter(|release| {
                filter.as_ref().is_none_or(|filter| {
                    filter.matches(&format!(
                        "{} {} {} {} {} {} {}",
                        release.namespace,
                        release.name,
                        release.revision,
                        release.status,
                        release.chart_name,
                        release.chart_version,
                        release.storage.name()
                    ))
                })
            })
            .collect::<Vec<_>>();
        if self.parent.is_some() && self.revision_sort {
            rows.sort_by_key(|release| std::cmp::Reverse(release.revision));
        } else {
            rows.sort_by(|a, b| {
                (&a.namespace, &a.name, a.storage)
                    .cmp(&(&b.namespace, &b.name, b.storage))
                    .then_with(|| b.revision.cmp(&a.revision))
            });
        }
        rows
    }
    pub fn key(&mut self, key: KeyCode) -> Action {
        if let Some(draft) = &mut self.draft {
            match key {
                KeyCode::Esc => self.draft = None,
                KeyCode::Enter => match super::nav::Filter::parse(draft) {
                    Ok(_) => {
                        self.filter = self.draft.take().unwrap_or_default();
                        self.selected = 0;
                    }
                    Err(_) => {
                        return Action::Unavailable(
                            "Invalid Helm filter; use a valid regex, !inverse, or -f fuzzy pattern.",
                        );
                    }
                },
                KeyCode::Backspace => {
                    draft.pop();
                }
                KeyCode::Char(c) if !c.is_control() && draft.len() + c.len_utf8() <= 512 => {
                    draft.push(c)
                }
                _ => {}
            }
            return Action::None;
        }
        match key {
            KeyCode::Esc | KeyCode::Char('q') if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
            KeyCode::Char('/') => self.draft = Some(self.filter.clone()),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.rows().len().saturating_sub(1)
            }
            KeyCode::Char('N') => {
                self.revision_sort = !self.revision_sort;
                self.selected = 0;
            }
            KeyCode::Char('i') => return Action::Lifecycle(Box::new(Intent::Install)),
            KeyCode::Char('c') => return Action::Lifecycle(Box::new(Intent::InstallCrds)),
            KeyCode::Char('t') => {
                if let Some(current) = self.current() {
                    return Action::Lifecycle(Box::new(Intent::Test(current)));
                }
            }
            KeyCode::Char('u') => {
                if let Some(current) = self.current() {
                    return Action::Lifecycle(Box::new(Intent::Upgrade(current)));
                }
            }
            KeyCode::Char('r') if self.parent.is_some() => {
                if let (Some(current), Some(target)) =
                    (self.parent.clone(), self.selected().cloned())
                {
                    return Action::Lifecycle(Box::new(Intent::Rollback {
                        current,
                        target: Box::new(target),
                    }));
                }
            }
            KeyCode::Enter | KeyCode::Char('r' | 'v' | 'd' | 'm' | 'y') => {
                if let Some(release) = self.selected().cloned() {
                    return match key {
                        KeyCode::Enter if self.parent.is_none() => {
                            Action::Request(Request::History(release))
                        }
                        KeyCode::Char('r') => Action::Request(Request::History(release)),
                        KeyCode::Enter | KeyCode::Char('v') => {
                            Action::Request(Request::Values(release))
                        }
                        KeyCode::Char('d') => Action::Request(Request::Detail(release)),
                        KeyCode::Char('m') => Action::Request(Request::Resources(release)),
                        _ => Action::Metadata(release),
                    };
                }
            }
            KeyCode::Delete => {
                if let Some(current) = self.current() {
                    return Action::Lifecycle(Box::new(Intent::Uninstall(current)));
                }
            }
            _ => {}
        }
        Action::None
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let rows = self.rows();
        let mut lines = vec![if let Some(parent) = &self.parent {
            format!(
                "{}/{} | {} storage | {} revisions | {}",
                parent.namespace,
                parent.name,
                parent.storage.name(),
                rows.len(),
                self.snapshot.source
            )
        } else {
            format!(
                "Namespace {} | {} releases | {} | {}",
                self.namespace,
                rows.len(),
                self.snapshot.source,
                if self.snapshot.truncated {
                    "INCOMPLETE"
                } else {
                    "bounded metadata"
                }
            )
        }];
        let issue_count = self.snapshot.issues.len();
        if issue_count > 0 {
            lines.push(format!(
                "ATTENTION: {issue_count} storage/read issues | {}",
                self.snapshot.issues[0]
            ));
        }
        if let Some(draft) = &self.draft {
            lines.push(format!(
                "/{}_| Enter filter | Esc cancel",
                tail(draft, width.saturating_sub(35))
            ));
        } else if !self.filter.is_empty() {
            lines.push(format!("FILTER /{} | Esc clear", self.filter));
        }
        let wide = width >= 114;
        let medium = width >= 64;
        let revision_width = rows
            .iter()
            .map(|release| release.revision.to_string().len())
            .max()
            .unwrap_or(3)
            .max(3);
        let name_width = if wide {
            26usize.saturating_sub(revision_width - 3)
        } else if medium {
            width.saturating_sub(25 + revision_width)
        } else {
            width.saturating_sub(14 + revision_width)
        }
        .max(4);
        lines.push(if wide{"   NAMESPACE        NAME                       REV STATUS           CHART                        APP       STORAGE"}else if medium{"   NAMESPACE / NAME                          REV STATUS     STORAGE"}else{"   NAME         REV ST STATE (S/CM)"}.into());
        let reserved = lines.len() + 1;
        let capacity = height.saturating_sub(reserved).max(1);
        let start = self.selected.saturating_sub(capacity.saturating_sub(1));
        if rows.is_empty() {
            lines.push("No releases match this view. Ctrl-R refreshes metadata.".into());
        }
        for (index, release) in rows.iter().enumerate().skip(start).take(capacity) {
            let mark = if index == self.selected { " >" } else { "  " };
            lines.push(if wide {
                format!(
                    "{mark} {:<16} {:<name_width$} {:>revision_width$} {:<16} {:<28} {:<9} {}",
                    clip(&release.namespace, 16),
                    clip(&release.name, name_width),
                    release.revision,
                    clip(&release.status, 16),
                    clip(
                        &format!("{}-{}", release.chart_name, release.chart_version),
                        28
                    ),
                    clip(&release.app_version, 9),
                    release.storage.name()
                )
            } else if medium {
                format!(
                    "{mark} {:<name_width$} {:>revision_width$} {:<10} {}",
                    clip(
                        &format!("{}/{}", release.namespace, release.name),
                        name_width
                    ),
                    release.revision,
                    clip(&release.status, 10),
                    release.storage.name()
                )
            } else {
                format!(
                    "{mark} {:<name_width$} {:>revision_width$} {:<2} {}",
                    clip(&release.name, name_width),
                    release.revision,
                    if release.storage == Storage::Secret {
                        "S"
                    } else {
                        "CM"
                    },
                    clip(&release.status, 6)
                )
            });
        }
        lines
    }
}
fn clip(text: &str, width: usize) -> String {
    safety::safe_text(text, width).chars().take(width).collect()
}
fn tail(text: &str, width: usize) -> String {
    text.chars()
        .rev()
        .take(width)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(name: &str, revision: u32, storage: Storage) -> Release {
        Release {
            namespace: "default".into(),
            name: name.into(),
            revision,
            status: "deployed".into(),
            chart_name: "web".into(),
            chart_version: "1.0".into(),
            app_version: "2".into(),
            updated: "now".into(),
            storage,
            uid: format!("uid-{name}-{revision}"),
            resource_version: "1".into(),
        }
    }
    fn snapshot(releases: Vec<Release>) -> Snapshot {
        Snapshot {
            releases,
            truncated: false,
            issues: vec![],
            source: "test".into(),
        }
    }
    fn demo_review() -> Result<Review> {
        let prepared = crate::cli::runtime()?.block_on(async {
            let gateway = Gateway::connect(crate::gateway::Connection {
                demo: true,
                writable: true,
                allow_helm: true,
                allow_helm_write: true,
                ..Default::default()
            })
            .await?;
            crate::helm_lifecycle::prepare(
                &gateway,
                crate::helm_lifecycle::Options {
                    executable: "/demo/helm".into(),
                    namespace: "demo".into(),
                    release: "demo-new".into(),
                    storage: Storage::Secret,
                    seconds: 300,
                    allow_hooks: false,
                    allow_crds: false,
                    registry: Default::default(),
                    keyring: None,
                    sigstore_policy: None,
                },
                crate::helm_lifecycle::Request::Install {
                    chart: "/demo/chart.tgz".into(),
                    values: None,
                },
            )
            .await
        })?;
        Ok(Review::new(Box::new(prepared), 7))
    }
    #[test]
    fn helm_review_uses_escaped_text_for_both_layout_and_approval() -> Result<()> {
        let mut review = demo_review()?;
        // Exercise the same cached presentation path with hostile manifest text.
        review.text = safety::review_display("spec: 東京\\u{e9}")?;
        review.typed = review.prepared.confirmation();
        let lines = review.lines(26, 100);
        assert!(lines.iter().all(|line| line.is_ascii() && line.len() <= 26));
        assert!(
            lines
                .iter()
                .map(|line| line.trim_start())
                .collect::<String>()
                .contains("\\u{6771}\\u{4eac}\\\\u{e9}")
        );
        review.mark_drawn(26, 2);
        assert!(!review.can_execute(28, 10, 7));
        while review.remaining(26) > 0 {
            review.key(KeyCode::PageDown, 26, 2);
            review.mark_drawn(26, 2);
        }
        assert!(review.can_execute(28, 10, 7));
        review.display_valid = false;
        assert!(!review.can_execute(28, 10, 7));
        Ok(())
    }

    #[test]
    fn review_requires_every_painted_line_and_resets_for_resize_and_epoch() -> Result<()> {
        let mut review = demo_review()?;
        review.typed = review.prepared.confirmation();
        assert!(!review.can_execute(28, 10, 7));
        review.mark_drawn(26, 2);
        assert!(review.remaining(26) > 0);
        review.key(KeyCode::End, 26, 2);
        review.mark_drawn(26, 2);
        assert!(
            review.remaining(26) > 0,
            "jumping to the last page cannot acknowledge skipped lines"
        );
        let count = review.wrapped(26).len();
        for offset in (0..count).step_by(2) {
            review.offset = offset;
            review.mark_drawn(26, 2);
        }
        assert!(review.can_execute(28, 10, 7));
        assert!(!review.can_execute(28, 10, 8));
        assert!(!review.can_execute(27, 10, 7));
        assert!(!review.can_execute(80, 24, 7));
        review.mark_drawn(78, 1);
        assert!(
            review.remaining(78) > 0,
            "new wrapping starts a fresh coverage record"
        );
        Ok(())
    }
    #[test]
    fn pinned_oci_form_preserves_source_and_explicit_extension_policy() -> Result<()> {
        let pinned = format!(
            "oci://registry.example:5443/team/chart@sha256:{}",
            "a".repeat(64)
        );
        let mut form = Form::new(
            Intent::Install,
            "staging".into(),
            Storage::Secret,
            None,
            false,
        );
        form.fields[1].1 = "my.api".into();
        form.fields[2].1 = pinned.clone();
        let default = form.draft("/trusted/helm".into(), 300)?;
        assert!(!default.options.allow_hooks);
        assert!(!default.options.allow_crds);
        form.set_lifecycle_policy(
            true,
            true,
            crate::helm_oci::Options {
                ca_file: Some("/private/registry-ca.pem".into()),
                auth_file: Some("/private/registry-auth.json".into()),
            },
        );
        let draft = form.draft("/trusted/helm".into(), 300)?;
        assert!(draft.options.allow_hooks && draft.options.allow_crds);
        assert_eq!(
            draft.options.registry.auth_file,
            Some("/private/registry-auth.json".into())
        );
        assert_eq!(
            draft.options.registry.ca_file,
            Some("/private/registry-ca.pem".into())
        );
        assert!(
            matches!(draft.request,crate::helm_lifecycle::Request::Install{chart,..} if chart.to_str()==Some(pinned.as_str()))
        );
        let lines = form.lines(82, 8).join("\n");
        assert!(lines.contains("hooks ON"));
        assert!(lines.contains("CRDs ON"));
        assert!(lines.contains("sha256"));
        assert!(
            !lines.contains("registry-auth.json"),
            "auth contents and private file paths are not part of form preview"
        );
        form.fields[2].1 = "oci://registry.example/team/chart:latest".into();
        assert!(form.draft("/trusted/helm".into(), 300).is_ok());
        Ok(())
    }
    #[test]
    fn chart_crds_are_separate_capability_gated_forms_without_values() -> Result<()> {
        let mut form = Form::new(
            Intent::InstallCrds,
            "staging".into(),
            Storage::Secret,
            Some(std::path::Path::new("/private/values.yaml")),
            true,
        );
        assert_eq!(form.intent.label(), "INSTALL CHART CRDS");
        assert_eq!(form.fields.len(), 3);
        form.fields[1].1 = "my.api".into();
        assert!(form.draft("/demo/helm".into(), 300).is_err());
        form.set_lifecycle_policy(false, true, Default::default());
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert!(matches!(
            draft.request,
            crate::helm_lifecycle::Request::InstallCrds { .. }
        ));
        assert_eq!(draft.options.namespace, "staging");
        assert_eq!(draft.options.release, "my.api");
        assert!(draft.expected.is_none());
        let lines = form.lines(82, 8).join("\n");
        assert!(lines.contains("no Helm release is installed"));
        assert!(!lines.contains("Values file"));
        Ok(())
    }
    #[test]
    fn forms_validate_explicit_targets_and_keep_active_path_tail_visible() -> Result<()> {
        let mut form = Form::new(Intent::Install, "*".into(), Storage::Secret, None, true);
        assert!(form.draft("/demo/helm".into(), 300).is_err());
        form.fields[0].1 = "demo".into();
        form.fields[1].1 = "my.api".into();
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert_eq!(draft.options.release, "my.api");
        assert!(matches!(
            draft.request,
            crate::helm_lifecycle::Request::Install { .. }
        ));
        form.fields[2].1 = "relative.tgz".into();
        assert!(form.draft("/demo/helm".into(), 300).is_err());
        form.fields[2].1 = format!("/{}-VISIBLE.tgz", "x".repeat(300));
        form.active = 2;
        assert!(form.lines(46, 5).join("\n").contains("VISIBLE.tgz_"));
        assert!(
            form.lines(26, 2).join("\n").contains("VISIBLE.tgz_"),
            "active input stays visible at minimum dialog height"
        );
        form.clear();
        assert!(form.fields[2].1.is_empty());
        Ok(())
    }
    #[test]
    fn configured_keyring_is_required_only_when_the_form_acquires_a_chart() {
        let current = release("web", 3, Storage::Secret);
        for intent in [
            Intent::Test(current.clone()),
            Intent::Rollback {
                current: current.clone(),
                target: Box::new(release("web", 1, Storage::Secret)),
            },
            Intent::Uninstall(current.clone()),
        ] {
            let mut form = Form::new(intent, "default".into(), Storage::Secret, None, true);
            form.set_keyring(Some("/private/trusted.gpg".into()));
            let lines = form.lines(98, 12).join("\n");
            assert!(lines.contains("signature n/a (stored release)"));
            assert!(!lines.contains("signature REQUIRED"));
        }
        let mut install = Form::new(
            Intent::Install,
            "default".into(),
            Storage::Secret,
            None,
            true,
        );
        assert!(
            install
                .lines(98, 12)
                .join("\n")
                .contains("signature not checked")
        );
        install.set_keyring(Some("/private/trusted.gpg".into()));
        assert!(
            install
                .lines(98, 12)
                .join("\n")
                .contains("signature REQUIRED")
        );
    }
    #[test]
    fn test_and_schema_actions_keep_explicit_capabilities_and_request_identity() -> Result<()> {
        let current = release("web", 3, Storage::Secret);
        let mut form = Form::new(
            Intent::Test(current.clone()),
            "ignored".into(),
            Storage::ConfigMap,
            None,
            true,
        );
        assert!(form.fields.is_empty());
        assert!(form.draft("/demo/helm".into(), 300).is_err());
        form.set_lifecycle_policy(true, false, Default::default());
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert!(matches!(
            draft.request,
            crate::helm_lifecycle::Request::Test
        ));
        assert_eq!(draft.options.namespace, current.namespace);
        assert_eq!(draft.options.storage, Storage::Secret);
        assert!(same_release(
            draft.expected.as_ref().unwrap_or(&current),
            &current
        ));
        for intent in [Intent::UpgradeCrds, Intent::DeleteCrds] {
            let mut form = Form::new(intent, "staging".into(), Storage::Secret, None, true);
            assert_eq!(form.fields.len(), 3);
            form.fields[1].1 = "schema-owner".into();
            assert!(form.draft("/demo/helm".into(), 300).is_err());
            form.set_lifecycle_policy(false, true, Default::default());
            let draft = form.draft("/demo/helm".into(), 300)?;
            assert!(matches!(
                draft.request,
                crate::helm_lifecycle::Request::UpgradeCrds { .. }
                    | crate::helm_lifecycle::Request::DeleteCrds { .. }
            ));
            assert!(draft.expected.is_none());
            assert!(!form.lines(98, 11).join("\n").contains("Values file"));
        }
        Ok(())
    }
    #[test]
    fn review_overview_preserves_the_complete_plan_and_coverage_requirement() -> Result<()> {
        let mut review = demo_review()?;
        let all = review.wrapped(118).join("\n");
        for section in [
            "OPERATION",
            "TARGET",
            "CONNECTION",
            "CHANGE PLAN",
            "APPROVAL",
            "FULL REVIEW",
        ] {
            assert!(all.contains(section));
        }
        let backend = super::super::wrap_text(review.prepared.summary(), 116)
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>();
        assert!(
            review.wrapped(118).ends_with(&backend),
            "all backend lines retain order and content after the consistent gutter"
        );
        assert!(all.contains("Simulated plan / no cluster writes"));
        review.typed = review.prepared.confirmation();
        review.mark_drawn(118, 3);
        let (painted, total) = review.progress(118);
        assert_eq!(painted, 3);
        assert!(total > painted && !review.can_execute(120, 32, 7));
        assert_eq!(review.remaining(118), total - painted);
        Ok(())
    }
    #[test]
    fn modern_forms_keep_active_input_tail_at_every_supported_size() {
        for (width, height) in [(26, 2), (46, 4), (78, 8), (98, 12)] {
            let mut form = Form::new(Intent::Install, "demo".into(), Storage::Secret, None, true);
            form.fields[2].1 = format!("/{}-VISIBLE.tgz", "x".repeat(300));
            form.active = 2;
            let lines = form.lines(width, height);
            assert!(lines.len() <= height);
            assert!(lines.join("\n").contains("VISIBLE.tgz_"));
        }
    }
    #[test]
    fn familiar_list_and_history_keys_capture_selected_revision() {
        let r = release("web", 3, Storage::Secret);
        let mut table = Table::new(
            snapshot(vec![r.clone()]),
            None,
            "default".into(),
            Some(Storage::Secret),
        );
        assert!(matches!(
            table.refresh(),
            Request::List(_, Some(Storage::Secret))
        ));
        assert!(
            matches!(table.key(KeyCode::Enter),Action::Request(Request::History(release)) if release.uid==r.uid)
        );
        let mut history = Table::new(
            snapshot(vec![release("web", 1, Storage::Secret), r.clone()]),
            Some(r.clone()),
            "default".into(),
            None,
        );
        assert_eq!(history.selected().map(|release| release.revision), Some(3));
        history.key(KeyCode::Down);
        assert!(
            matches!(history.key(KeyCode::Enter),Action::Request(Request::Values(release)) if release.revision==1)
        );
        assert!(matches!(
            history.key(KeyCode::Char('r')),
            Action::Lifecycle(intent) if matches!(*intent,Intent::Rollback { .. })
        ));
    }
    #[test]
    fn duplicate_drivers_require_explicit_selection() {
        let s = snapshot(vec![
            release("web", 1, Storage::Secret),
            release("web", 2, Storage::ConfigMap),
        ]);
        assert!(helm::select_release(&s, "web", None).is_err());
        assert_eq!(
            helm::select_release(&s, "web", Some(Storage::ConfigMap))
                .map(|release| release.revision),
            Ok(2)
        );
    }
    #[test]
    fn filter_cancel_and_compact_frames_are_bounded() {
        let mut table = Table::new(
            snapshot(vec![
                release("web", 1, Storage::Secret),
                release("api", 1, Storage::Secret),
            ]),
            None,
            "default".into(),
            None,
        );
        table.key(KeyCode::Char('/'));
        table.key(KeyCode::Char('w'));
        table.key(KeyCode::Esc);
        assert_eq!(table.rows().len(), 2);
        table.key(KeyCode::Char('/'));
        for c in "^default web ".chars() {
            table.key(KeyCode::Char(c));
        }
        table.key(KeyCode::Enter);
        assert_eq!(table.rows().len(), 1);
        assert!(matches!(table.key(KeyCode::Esc), Action::None));
        assert_eq!(table.rows().len(), 2);
        assert!(table.lines(26, 4).iter().all(|line| !line.contains('\x1b')));
    }
}
