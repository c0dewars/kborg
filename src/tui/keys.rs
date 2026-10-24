//! Keyboard handling for the table, modals and dashboard.
use super::*;

impl App {
    pub(super) fn key(&mut self, code: KeyCode) -> Result<bool> {
        self.key_with_approval_size(code, terminal::size)
    }
    pub(super) fn key_with_approval_size(
        &mut self,
        code: KeyCode,
        approval_size: impl FnOnce() -> io::Result<(u16, u16)>,
    ) -> Result<bool> {
        if matches!(self.modal, Modal::AdviceRunning) {
            self.status = "Copilot request running; Ctrl-C cancels and waits for cleanup.".into();
            return Ok(false);
        }
        if matches!(self.modal, Modal::HelmRunning(..)) {
            self.status="Helm is running asynchronously. Ctrl-C requests cancellation; wait for the final outcome.".into();
            return Ok(false);
        }
        if self.write_inflight {
            self.status =
                "Operation in progress; wait for the result before navigating or quitting.".into();
            return Ok(false);
        }
        if code == KeyCode::Char(':')
            && match &self.modal {
                Modal::None
                | Modal::Detail(..)
                | Modal::Overview(..)
                | Modal::PodContainers(..)
                | Modal::Metrics(..)
                | Modal::LiveOverview(..)
                | Modal::Directory(..)
                | Modal::ForwardView
                | Modal::HelmDocument(..)
                | Modal::AdviceDocument(..)
                | Modal::Help(..) => true,
                Modal::Follow(view) => !view.is_filtering(),
                Modal::Helm(table) => !table.is_filtering(),
                _ => false,
            }
        {
            let modal = std::mem::replace(&mut self.modal, Modal::None);
            if !matches!(modal, Modal::None) {
                self.push_subview(modal);
            }
            self.command_cursor = 0;
            self.history_index = None;
            self.completion = None;
            self.modal = Modal::Input("Command", String::new());
            return Ok(false);
        }
        self.sticky_status = false;
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::YamlEditor(mut editor) => {
                match editor.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)) {
                    Ok(crate::ui_yaml::Event::Cancel) => self.close_subview(),
                    Ok(crate::ui_yaml::Event::Review) => self.send(Task::YamlPrepare(editor)),
                    Ok(crate::ui_yaml::Event::Continue | crate::ui_yaml::Event::External) => {
                        self.modal = Modal::YamlEditor(editor)
                    }
                    Err(error) => {
                        self.status = error;
                        self.modal = Modal::YamlEditor(editor);
                    }
                }
            }
            Modal::WriteRunning => self.modal = Modal::WriteRunning,
            Modal::WriteReview(mut review) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if !self.busy && review.view.ready(width, height, self.epoch) {
                        if let WritePlan::Tool(prepared) = review.plan {
                            self.pending_tool = Some((prepared, review.view.typed));
                            self.status = "Running the reviewed tool…".into();
                            return Ok(false);
                        }
                        self.modal = Modal::WriteRunning;
                        let task = match review.plan {
                            WritePlan::Tool(_) => return Ok(false),
                            WritePlan::Yaml(prepared) => {
                                Task::YamlExecute(prepared, review.view.typed)
                            }
                            WritePlan::Node(prepared) => {
                                Task::NodeExecute(prepared, review.view.typed)
                            }
                            WritePlan::Apply(prepared) => {
                                Task::ApplyExecute(prepared, review.view.typed)
                            }
                            WritePlan::NodeShell(prepared) => {
                                Task::NodeShellStart(prepared, review.view.typed)
                            }
                            WritePlan::Debug(prepared) => {
                                Task::DebugStart(prepared, review.view.typed)
                            }
                        };
                        self.send(task);
                        if !self.write_inflight {
                            self.modal = Modal::Detail(
                                "Operation not queued. Prepare and review again.".into(),
                                0,
                            );
                        }
                    } else {
                        self.status =
                            "Read every review line and type the exact target before applying."
                                .into();
                        self.modal = Modal::WriteReview(review);
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.view.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::WriteReview(review);
                }
            },
            Modal::Export(draft) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    let review = safety::review_display(&draft.review());
                    if let Err(error) = review {
                        self.status = error;
                        self.modal = Modal::Export(draft);
                    } else if width < 28
                        || height < 10
                        || wrap_text(
                            &review.unwrap_or_default(),
                            usize::from(width.saturating_sub(2)),
                        )
                        .lines()
                        .count()
                            > usize::from(height.saturating_sub(8))
                    {
                        self.status="Enlarge the terminal to review the full export destination before saving.".into();
                        self.modal = Modal::Export(draft);
                    } else {
                        self.status = match draft.save() {
                            Ok(path) => format!(
                                "Saved {}",
                                path.display()
                                    .to_string()
                                    .chars()
                                    .flat_map(char::escape_default)
                                    .collect::<String>()
                            ),
                            Err(error) => error,
                        };
                        self.modal =
                            Modal::AdviceDocument("EXPORT RESULT".into(), self.status.clone(), 0);
                    }
                }
                _ => self.modal = Modal::Export(draft),
            },
            Modal::AdviceReview(mut review) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if self.busy || !review.can_send(width, height, self.epoch) {
                        self.status="Read every preview line and type the exact transfer confirmation. Local previews cannot send.".into();
                        self.modal = Modal::AdviceReview(review);
                    } else if let Some(prepared) = review.prepared.take() {
                        let (cancel, receiver) = tokio::sync::oneshot::channel();
                        self.advice_cancel = Some(cancel);
                        self.modal = Modal::AdviceRunning;
                        self.send(Task::AdviceExecute(
                            Box::new(prepared),
                            review.typed,
                            receiver,
                        ));
                        if !self.write_inflight {
                            self.advice_cancel = None;
                            self.modal = Modal::AdviceDocument(
                                "COPILOT NOT STARTED".into(),
                                "Request queue unavailable; prepare and review again.".into(),
                                0,
                            );
                        }
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::AdviceReview(review);
                }
            },
            Modal::AdviceLoading => {
                if !matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.modal = Modal::AdviceLoading;
                }
            }
            Modal::AdviceRunning => self.modal = Modal::AdviceRunning,
            Modal::AdviceDocument(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.close_subview();
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::AdviceDocument(title, text, offset);
                }
            }
            Modal::HelmForm(mut form) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let tool = self.options.helm_tool.clone().or_else(|| {
                        self.options
                            .connection
                            .demo
                            .then(|| std::path::PathBuf::from("/demo/helm"))
                    });
                    if self.busy {
                        self.status = "Wait for the current request; Helm draft retained.".into();
                        self.modal = Modal::HelmForm(form);
                    } else if let Some(tool) = tool {
                        match form.draft(tool, self.options.helm_timeout_seconds) {
                            Ok(draft) => {
                                self.helm_return =
                                    Some((draft.options.namespace.clone(), draft.options.storage));
                                self.push_subview(Modal::HelmForm(form));
                                self.helm_generation = self.helm_generation.wrapping_add(1);
                                self.modal = Modal::HelmLoading(
                                    "Validating local files and preparing sealed Helm review...",
                                );
                                self.status =
                                    "Preparing Helm review; no execution requested.".into();
                                self.send(Task::HelmPrepare(Box::new(draft), self.helm_generation));
                            }
                            Err(error) => {
                                self.status = error;
                                self.modal = Modal::HelmForm(form);
                            }
                        }
                    } else {
                        self.status =
                            "Helm tool path missing; restart with --helm-tool ABSOLUTE_PATH."
                                .into();
                        self.modal = Modal::HelmForm(form);
                    }
                }
                _ => {
                    form.key(code);
                    self.modal = Modal::HelmForm(form);
                }
            },
            Modal::HelmReview(mut review) => match code {
                KeyCode::Esc => {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                }
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if self.busy || !review.can_execute(width, height, self.epoch) {
                        self.status="Read every review line (PgUp/PgDown), keep this session, and type the exact confirmation shown below.".into();
                        self.modal = Modal::HelmReview(review);
                    } else {
                        let confirmation = review.prepared.confirmation();
                        let (cancel, receiver) = tokio::sync::oneshot::channel();
                        self.helm_cancel = Some(cancel);
                        self.helm_elapsed = 0;
                        self.modal = Modal::HelmRunning(confirmation, Instant::now(), false);
                        self.send(Task::HelmExecute(
                            review.prepared,
                            review.typed,
                            self.helm_generation,
                            receiver,
                        ));
                        if !self.write_inflight {
                            self.helm_cancel = None;
                            self.modal=Modal::HelmOutcome("HELM NOT STARTED".into(),"The command queue is unavailable; no Helm execution was queued. Prepare and review again.".into(),0);
                        } else {
                            self.status =
                                "Helm execution in progress; Ctrl-C requests cancellation.".into();
                        }
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::HelmReview(review);
                }
            },
            Modal::HelmRunning(target, started, cancelled) => {
                self.modal = Modal::HelmRunning(target, started, cancelled)
            }
            Modal::HelmOutcome(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    if let Some((namespace, storage)) = self.helm_return.take() {
                        self.request_helm(helm_ui::Request::List(namespace, Some(storage)), false);
                    } else {
                        self.close_subview();
                    }
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::HelmOutcome(title, text, offset);
                }
            }
            Modal::HelmLoading(label) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                } else {
                    self.modal = Modal::HelmLoading(label);
                }
            }
            Modal::Helm(mut table) => match table.key(code) {
                helm_ui::Action::Close => {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                }
                helm_ui::Action::Request(request) => {
                    self.modal = Modal::Helm(table);
                    self.request_helm(request, true);
                }
                helm_ui::Action::Lifecycle(intent) => {
                    self.modal = Modal::Helm(table);
                    self.begin_helm_form(*intent);
                }
                helm_ui::Action::Metadata(release) => {
                    let text = crate::inspect::yaml(
                        &serde_json::to_value(release)
                            .map_err(|_| "Helm metadata encoding failed")?,
                    )?;
                    self.push_subview(Modal::Helm(table));
                    self.modal =
                        Modal::HelmDocument("HELM SAFE REVISION METADATA / YAML".into(), text, 0);
                }
                helm_ui::Action::Unavailable(reason) => {
                    self.status = reason.into();
                    self.modal = Modal::Helm(table);
                }
                helm_ui::Action::None => self.modal = Modal::Helm(table),
            },
            Modal::HelmDocument(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.close_subview();
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::HelmDocument(title, text, offset);
                }
            }
            Modal::ForwardView => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.forward_selected = (self.forward_selected + 1)
                        .min(self.worker.forward.len().saturating_sub(1));
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.forward_selected = self.forward_selected.saturating_sub(1);
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Char('s') => {
                    self.status = if self.worker.forward.cancel_selected(self.forward_selected) {
                        "Stopping selected forward; waiting for listener and client cleanup."
                    } else {
                        "Selected forward is already stopped or stopping."
                    }
                    .into();
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Char('F') => {
                    self.modal = Modal::ForwardView;
                    self.request_forward();
                }
                KeyCode::Char('b') => {
                    self.modal = Modal::ForwardView;
                    self.bench(200, 5, "/");
                }
                _ => self.modal = Modal::ForwardView,
            },
            Modal::ForwardForm(namespace, pod, uid, mut fields, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Tab | KeyCode::Down => {
                    index = (index + 1) % 3;
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::BackTab | KeyCode::Up => {
                    index = if index == 0 { 2 } else { index - 1 };
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Backspace => {
                    fields[index].pop();
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Char(c) if c.is_ascii_digit() && fields[index].len() < 5 => {
                    fields[index].push(c);
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Enter => {
                    let parsed = fields
                        .iter()
                        .map(|field| field.parse::<u16>())
                        .collect::<std::result::Result<Vec<_>, _>>();
                    if self.busy {
                        self.status =
                            "Wait for the current request; forwarding draft retained.".into();
                        self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                    } else if let Ok(ports) = parsed
                        && ports[0] >= 1024
                        && ports[1] > 0
                        && (1..=300).contains(&ports[2])
                        && uid.is_some()
                    {
                        self.send(Task::PrepareSession(
                            namespace,
                            pod,
                            uid,
                            crate::ops::Request::PortForward {
                                local_port: ports[0],
                                remote_port: ports[1],
                                seconds: ports[2],
                            },
                        ));
                    } else {
                        self.status="Use local port 1024..65535, remote port 1..65535 and duration 1..300s; Pod UID required.".into();
                        self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                    }
                }
                _ => self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index),
            },
            Modal::Subjects(mut view) => match view.key(code) {
                crate::ui_subjects::Outcome::Close => self.close_subview(),
                crate::ui_subjects::Outcome::Stay => self.modal = Modal::Subjects(view),
                crate::ui_subjects::Outcome::Open(subject) => {
                    let namespace = view.namespace.clone();
                    self.push_subview(Modal::Subjects(view));
                    self.send(Task::Can(namespace, subject));
                }
            },
            Modal::Secrets(mut view) => match view.key(code) {
                crate::ui_secrets::Outcome::Close => self.close_subview(),
                crate::ui_secrets::Outcome::Stay => self.modal = Modal::Secrets(view),
                crate::ui_secrets::Outcome::Reveal(namespace, name, uid, key) => {
                    self.modal = Modal::Secrets(view);
                    self.send(Task::SecretValue(namespace, name, uid, key));
                }
            },
            Modal::Directory(directory, mut index) => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(directory.entries.len().saturating_sub(1));
                    self.modal = Modal::Directory(directory, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Directory(directory, index.saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char('y' | 'v') => {
                    if let Some(entry) = directory.entries.get(index) {
                        let path = entry.path.clone();
                        let is_directory = entry.directory;
                        self.push_subview(Modal::Directory(directory, index));
                        self.send(if is_directory {
                            Task::Directory(path)
                        } else {
                            Task::Manifest(path)
                        });
                    }
                }
                KeyCode::Char('a') => {
                    if let Some(entry) = directory
                        .entries
                        .get(index)
                        .filter(|entry| !entry.directory)
                    {
                        let path = entry.path.clone();
                        self.push_subview(Modal::Directory(directory, index));
                        self.apply_manifest(path);
                    } else {
                        self.modal = Modal::Directory(directory, index);
                    }
                }
                _ => self.modal = Modal::Directory(directory, index),
            },
            Modal::PodContainers(object, mut index) => {
                let names = pod_log_choices(&object);
                match code {
                    KeyCode::Char('s' | 'a') => {
                        if names.get(index).is_some_and(|name| name == "*") {
                            self.status = "Choose a specific container for shell or attach.".into();
                            self.modal = Modal::PodContainers(object, index);
                            return Ok(false);
                        }
                        let attach = code == KeyCode::Char('a');
                        if self.busy
                            || !self.options.connection.writable
                            || !(if attach {
                                self.options.connection.allow_attach
                            } else {
                                self.options.connection.allow_exec
                            })
                            || self.options.connection.demo
                        {
                            self.status = if attach {
                                "Attach requires a live Pod, --write and --allow-attach."
                            } else {
                                "Shell requires a live Pod, --write and --allow-exec."
                            }
                            .into();
                            self.modal = Modal::PodContainers(object, index);
                        } else if let Some(container) = names.get(index) {
                            let namespace = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let uid = object["metadata"]["uid"].as_str().map(str::to_owned);
                            let request = if attach {
                                crate::ops::Request::InteractiveAttach {
                                    container: container.clone(),
                                    seconds: self.options.session_seconds,
                                }
                            } else {
                                crate::ops::Request::InteractiveExec {
                                    container: container.clone(),
                                    argv: vec!["/bin/sh".into(), "-i".into()],
                                    seconds: self.options.session_seconds,
                                }
                            };
                            self.push_subview(Modal::PodContainers(object, index));
                            self.send(Task::PrepareSession(namespace, pod, uid, request));
                        }
                    }
                    KeyCode::Char('f') => {
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::ForwardView;
                    }
                    KeyCode::Char('F') => {
                        if self.busy
                            || !self.options.connection.writable
                            || !self.options.connection.allow_port_forward
                            || self.options.connection.demo
                        {
                            self.status =
                                "Port-forward requires a live Pod, --write and --allow-forward."
                                    .into();
                            self.modal = Modal::PodContainers(object, index);
                        } else {
                            let namespace = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let uid = object["metadata"]["uid"].as_str().map(str::to_owned);
                            self.push_subview(Modal::PodContainers(object, index));
                            self.modal = Modal::ForwardForm(
                                namespace,
                                pod,
                                uid,
                                [
                                    self.worker.forward.suggested_port().to_string(),
                                    "80".into(),
                                    "300".into(),
                                ],
                                0,
                            );
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                    KeyCode::Down | KeyCode::Char('j') => {
                        index = (index + 1).min(names.len().saturating_sub(1));
                        self.modal = Modal::PodContainers(object, index);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.modal = Modal::PodContainers(object, index.saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('L' | 'p') => {
                        if !self.options.connection.logs {
                            self.status = "Logs require --allow-logs.".into();
                            self.modal = Modal::PodContainers(object, index);
                        } else if let Some(container) = names.get(index) {
                            let n = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let container = container.clone();
                            self.push_subview(Modal::PodContainers(object, index));
                            let uid = self.subviews.last().and_then(|modal| {
                                if let Modal::PodContainers(object, _) = modal {
                                    object["metadata"]["uid"].as_str().map(str::to_owned)
                                } else {
                                    None
                                }
                            });
                            if let Some(uid) = uid {
                                self.open_logs(
                                    LogTarget {
                                        namespace: n,
                                        pod,
                                        all: container == "*",
                                        dynamic: false,
                                        membership: None,
                                        container: if container == "*" {
                                            self.subviews.last().and_then(|modal| {
                                                if let Modal::PodContainers(object, _) = modal {
                                                    ordinary_container_names(object)
                                                        .into_iter()
                                                        .next()
                                                } else {
                                                    None
                                                }
                                            })
                                        } else {
                                            Some(container)
                                        },
                                        uid,
                                        previous: matches!(code, KeyCode::Char('L' | 'p')),
                                        follow: !matches!(code, KeyCode::Char('L' | 'p')),
                                    },
                                    self.options.log_read,
                                );
                            } else {
                                self.status =
                                    "Pod UID unavailable; refresh before reading logs.".into();
                                self.close_subview();
                            }
                        }
                    }
                    KeyCode::Char('d') => {
                        let text = overview_text(&object, &self.context);
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::Detail(text, 0);
                    }
                    KeyCode::Char('y' | 'v') => {
                        let text = crate::inspect::yaml(&object)?;
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::Detail(text, 0);
                    }
                    _ => self.modal = Modal::PodContainers(object, index),
                }
            }
            Modal::EditContainers(k, n, name, uid, mut values, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(values.len().saturating_sub(1));
                    self.modal = Modal::EditContainers(k, n, name, uid, values, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    index = index.saturating_sub(1);
                    self.modal = Modal::EditContainers(k, n, name, uid, values, index);
                }
                KeyCode::Enter if index < values.len() => {
                    let value = values.remove(index);
                    self.modal = Modal::Editor(Box::new(ContainerDraft {
                        kind: k,
                        namespace: n,
                        name,
                        uid,
                        container: value.name,
                        fields: [
                            value.image,
                            value.resources.cpu_request,
                            value.resources.memory_request,
                            value.resources.cpu_limit,
                            value.resources.memory_limit,
                        ],
                        active: 0,
                        resources: false,
                    }));
                }
                _ => self.modal = Modal::EditContainers(k, n, name, uid, values, index),
            },
            Modal::Editor(mut draft) => match code {
                KeyCode::Esc => {}
                KeyCode::F(2) => {
                    draft.resources = !draft.resources;
                    if !draft.resources {
                        draft.active = 0;
                    }
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Tab | KeyCode::Down => {
                    draft.active = (draft.active + 1) % if draft.resources { 5 } else { 1 };
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::BackTab | KeyCode::Up => {
                    draft.active = if draft.active == 0 {
                        if draft.resources { 4 } else { 0 }
                    } else {
                        draft.active - 1
                    };
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Backspace => {
                    draft.fields[draft.active].pop();
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Char(c)
                    if c.is_ascii_graphic()
                        && draft.fields[draft.active].len()
                            < if draft.active == 0 { 512 } else { 128 } =>
                {
                    draft.fields[draft.active].push(c);
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Enter => {
                    let edit = crate::edit::ContainerEdit {
                        image: draft.fields[0].clone(),
                        resources: draft.resources.then(|| crate::edit::ResourceEdit {
                            cpu_request: draft.fields[1].clone(),
                            memory_request: draft.fields[2].clone(),
                            cpu_limit: draft.fields[3].clone(),
                            memory_limit: draft.fields[4].clone(),
                        }),
                    };
                    if self.busy {
                        self.status = "Busy; draft retained. Wait before preparing.".into();
                        self.modal = Modal::Editor(draft);
                    } else if let Err(error) = edit.validate(&draft.container) {
                        self.status = error;
                        self.modal = Modal::Editor(draft);
                    } else {
                        self.send(Task::Prepare(
                            draft.kind,
                            draft.namespace,
                            draft.name,
                            draft.uid,
                            Action::SetContainer {
                                container: draft.container,
                                edit,
                            },
                        ));
                    }
                }
                _ => self.modal = Modal::Editor(draft),
            },
            Modal::Filter(original, mut text, original_uid, original_index) => match code {
                KeyCode::Esc => {
                    self.filter = original;
                    self.selected = original_uid
                        .and_then(|uid| {
                            self.visible().iter().position(|object| {
                                object["metadata"]["uid"].as_str() == Some(uid.as_str())
                            })
                        })
                        .unwrap_or_else(|| {
                            original_index.min(self.visible().len().saturating_sub(1))
                        });
                }
                KeyCode::Enter => {
                    if let Some(selector) = text.strip_prefix("-l ") {
                        self.push_view();
                        self.selector = selector.trim().into();
                        self.filter.clear();
                        self.changed();
                    } else if let Err(error) = nav::Filter::parse(&text) {
                        self.status = error;
                        self.modal = Modal::Filter(original, text, original_uid, original_index);
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                KeyCode::Char(c) if !c.is_control() && text.len() + c.len_utf8() <= 512 => {
                    text.push(c);
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                _ => self.modal = Modal::Filter(original, text, original_uid, original_index),
            },
            Modal::Follow(mut view) => {
                use crate::ui_logs::LogAction;
                if self.busy && !view.is_filtering() && matches!(code, KeyCode::Char('0'..='6')) {
                    self.status = "Wait for the current log request before changing range.".into();
                    self.modal = Modal::Follow(view);
                    return Ok(false);
                }
                match view.key(code) {
                    LogAction::Close => {
                        self.worker.logs.cancel();
                        self.log_target = None;
                        self.close_subview();
                    }
                    LogAction::Reload(read) => self.reload_logs(view, read),
                    LogAction::AllContainers => {
                        if self.busy {
                            self.status =
                                "Wait for the current request before changing log sources.".into();
                            self.modal = Modal::Follow(view);
                        } else if let Some(target) = &mut self.log_target {
                            if target.all && target.container.is_none() {
                                self.status="No preferred single container; choose one from the Pod container view.".into();
                                self.modal = Modal::Follow(view);
                            } else {
                                target.all = !target.all;
                                view.replace_source("Connecting to selected log sources...".into());
                                let read = view.range;
                                self.reload_logs(view, read);
                            }
                        } else {
                            self.modal = Modal::Follow(view);
                        }
                    }
                    LogAction::Clear => {
                        if let Ok(mut state) = self.worker.logs.state.lock() {
                            state.buffer = crate::logs::RollingLog::default();
                            state.dirty = false;
                        }
                        self.modal = Modal::Follow(view);
                    }
                    LogAction::Mark => {
                        if view.following {
                            let generation = self
                                .worker
                                .logs
                                .generation
                                .load(std::sync::atomic::Ordering::SeqCst);
                            self.worker.logs.append(
                                generation,
                                "[---------------- KBorg log mark ----------------]",
                            );
                        }
                        self.modal = Modal::Follow(view);
                    }
                    LogAction::None => self.modal = Modal::Follow(view),
                }
            }
            Modal::Containers(target, names, mut index) => match code {
                KeyCode::Esc => {
                    self.worker.logs.cancel();
                    self.close_subview();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(names.len().saturating_sub(1));
                    self.modal = Modal::Containers(target, names, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Containers(target, names, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        let mut target = target;
                        target.all = container == "*";
                        if !target.all {
                            target.container = Some(container.clone());
                        }
                        self.open_logs(target, self.options.log_read);
                    }
                }
                _ => self.modal = Modal::Containers(target, names, index),
            },
            Modal::Image(k, n, name, uid, container, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Some(container) = container {
                        self.send(Task::Prepare(
                            k,
                            n,
                            name,
                            uid,
                            Action::SetImage {
                                container,
                                image: text,
                            },
                        ));
                    } else if safety::valid_id(&text) {
                        self.modal = Modal::Image(k, n, name, uid, Some(text), String::new());
                    } else {
                        self.status = "Enter a valid deployment container name.".into();
                        self.modal = Modal::Image(k, n, name, uid, None, text);
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Image(k, n, name, uid, container, text);
                }
                KeyCode::Char(c) if c.is_ascii_graphic() && text.len() < 512 => {
                    text.push(c);
                    self.modal = Modal::Image(k, n, name, uid, container, text);
                }
                _ => self.modal = Modal::Image(k, n, name, uid, container, text),
            },
            Modal::Input("Command", mut text) => {
                self.command_cursor = self.command_cursor.min(text.len());
                match code {
                    KeyCode::Esc => {
                        self.completion = None;
                        self.history_index = None;
                        self.close_subview();
                    }
                    KeyCode::Enter => {
                        self.remember_command(&text);
                        return self.command(&text);
                    }
                    KeyCode::Up => {
                        if !self.history.is_empty() {
                            let index = self
                                .history_index
                                .map(|i| i.saturating_sub(1))
                                .unwrap_or_else(|| {
                                    self.history_draft = text.clone();
                                    self.history.len() - 1
                                });
                            self.history_index = Some(index);
                            text = self.history[index].clone();
                            self.command_cursor = text.len();
                            self.completion = None;
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Down => {
                        if let Some(index) = self.history_index {
                            if index + 1 < self.history.len() {
                                self.history_index = Some(index + 1);
                                text = self.history[index + 1].clone();
                            } else {
                                self.history_index = None;
                                text = self.history_draft.clone();
                            }
                            self.command_cursor = text.len();
                            self.completion = None;
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        let (base, values, index) = self.completion.take().unwrap_or_else(|| {
                            (
                                text.clone(),
                                self.suggestions(&text),
                                if code == KeyCode::BackTab {
                                    usize::MAX
                                } else {
                                    0
                                },
                            )
                        });
                        if !values.is_empty() {
                            let index = if index == usize::MAX {
                                values.len() - 1
                            } else {
                                index % values.len()
                            };
                            text = values[index].clone();
                            self.command_cursor = text.len();
                            self.completion = Some((
                                base,
                                values.clone(),
                                if code == KeyCode::BackTab {
                                    index.checked_sub(1).unwrap_or(values.len() - 1)
                                } else {
                                    (index + 1) % values.len()
                                },
                            ));
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Left => {
                        self.command_cursor = text[..self.command_cursor]
                            .char_indices()
                            .next_back()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Right => {
                        self.command_cursor = text[self.command_cursor..]
                            .chars()
                            .next()
                            .map(|c| self.command_cursor + c.len_utf8())
                            .unwrap_or(text.len());
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Home => {
                        self.command_cursor = 0;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::End => {
                        self.command_cursor = text.len();
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Backspace => {
                        if let Some((previous, _)) =
                            text[..self.command_cursor].char_indices().next_back()
                        {
                            text.drain(previous..self.command_cursor);
                            self.command_cursor = previous;
                        }
                        self.completion = None;
                        self.history_index = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Delete => {
                        if let Some(c) = text[self.command_cursor..].chars().next() {
                            text.drain(self.command_cursor..self.command_cursor + c.len_utf8());
                        }
                        self.completion = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Char(c)
                        if !c.is_control() && text.len() + c.len_utf8() <= nav::COMMAND_BYTES =>
                    {
                        text.insert(self.command_cursor, c);
                        self.command_cursor += c.len_utf8();
                        self.completion = None;
                        self.history_index = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    _ => self.modal = Modal::Input("Command", text),
                }
            }
            Modal::Input(label, mut text) => match code {
                KeyCode::Esc => {
                    if matches!(
                        label,
                        selections_ui::SAVE_LABEL | selections_ui::DELETE_LABEL
                    ) {
                        self.selection_ui.draft = None;
                        self.selection_ui.deleting = None;
                        self.restore_selection_view();
                    }
                }
                KeyCode::Enter => {
                    if label == selections_ui::SAVE_LABEL {
                        self.commit_selection_name(&text);
                    } else if label == selections_ui::DELETE_LABEL {
                        self.delete_selection_name(&text);
                    } else if label == "Command" {
                        return self.command(&text);
                    } else if label == "Filter" {
                        self.filter = text;
                        self.selected = 0;
                    } else if label == OVERVIEW_CONTEXTS_LABEL {
                        let names: Vec<String> =
                            text.split(',').map(|name| name.trim().to_owned()).collect();
                        if names.len() > crate::overview::MAX_LIVE_CONTEXTS {
                            self.status="The dashboard allows up to 9 explicit contexts; CLI snapshots support up to 100.".into();
                        } else if let Err(error) = crate::overview::validate(&names) {
                            self.status = error;
                        } else {
                            self.open_dashboard(names)?;
                        }
                    } else if label == "Namespace" {
                        if text == "*" || safety::valid_id(&text) {
                            self.namespace = text;
                            self.namespace_override = true;
                            self.desired_resource = None;
                            if let Ok(index) = self.resolve_resource("v1/pods") {
                                self.kind_index = index;
                            }
                            self.changed();
                        } else {
                            self.status = "Invalid namespace.".into();
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Input(label, text);
                }
                KeyCode::Char(c) => {
                    if !c.is_control()
                        && text.len().saturating_add(c.len_utf8()) <= input_limit(label)
                    {
                        text.push(c);
                    }
                    self.modal = Modal::Input(label, text);
                }
                _ => self.modal = Modal::Input(label, text),
            },
            Modal::Detail(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                _ => {
                    offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::Detail(text, offset);
                }
            },
            Modal::Overview(object, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('J') => {
                    self.modal = Modal::Detail(
                        serde_json::to_string_pretty(&object)
                            .map_err(|_| "serialization failed")?,
                        0,
                    )
                }
                KeyCode::Char('y') => self.modal = Modal::Detail(crate::inspect::yaml(&object)?, 0),
                _ => {
                    offset = scroll_offset(&overview_text(&object, &self.context), offset, code);
                    self.modal = Modal::Overview(object, offset);
                }
            },
            Modal::LiveOverview(mut dashboard) => {
                dashboard.notice = None;
                if let Some(picker) = dashboard.picker.as_mut() {
                    match picker.key(code) {
                        dashboard_ui::PickerOutcome::Stay => {}
                        dashboard_ui::PickerOutcome::Cancel => dashboard.picker = None,
                        dashboard_ui::PickerOutcome::Start(chosen) => {
                            dashboard.picker = None;
                            self.start_dashboard_tail(&mut dashboard, chosen);
                        }
                    }
                    self.modal = Modal::LiveOverview(dashboard);
                    return Ok(false);
                }
                if let (Some(pane), KeyCode::Char('e' | 'W')) = (dashboard.logs.as_mut(), code) {
                    pane.view.key(code);
                    self.modal = Modal::LiveOverview(dashboard);
                    return Ok(false);
                }
                match code {
                    KeyCode::Char('?') => dashboard.help = !dashboard.help,
                    KeyCode::Esc if dashboard.help => dashboard.help = false,
                    KeyCode::Esc if dashboard.logs.is_some() => {
                        dashboard.logs = None;
                        self.worker.logs.cancel();
                    }
                    KeyCode::Esc if dashboard.warning.is_some() => dashboard.warning = None,
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if dashboard.logs.is_some() {
                            self.worker.logs.cancel();
                        }
                        self.overview = None;
                        return Ok(false);
                    }
                    KeyCode::Char('L') if !dashboard.help => self.dashboard_logs(&mut dashboard),
                    KeyCode::Char('t') if !dashboard.help => self.dashboard_tail(&mut dashboard),
                    KeyCode::Enter if !dashboard.help => {
                        if dashboard.logs.take().is_some() {
                            self.worker.logs.cancel();
                        }
                        self.open_dashboard_pods(dashboard)?;
                        return Ok(false);
                    }
                    KeyCode::Char('c') if !dashboard.help => {
                        self.overview = None;
                        self.modal = Modal::Input(
                            OVERVIEW_CONTEXTS_LABEL,
                            dashboard
                                .rows
                                .iter()
                                .map(|row| row.context.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                        );
                        return Ok(false);
                    }
                    KeyCode::Char('s') if !dashboard.help => {
                        self.modal = Modal::LiveOverview(dashboard);
                        self.save_dashboard_selection()?;
                        return Ok(false);
                    }
                    KeyCode::Char('B') if !dashboard.help => {
                        self.modal = Modal::LiveOverview(dashboard);
                        self.open_saved_selections()?;
                        return Ok(false);
                    }
                    _ if !dashboard.help => dashboard.key(code),
                    _ => {}
                }
                self.modal = Modal::LiveOverview(dashboard);
            }
            Modal::SessionContainers(n, name, uid, names, index, attach) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal = Modal::SessionContainers(
                        n,
                        name,
                        uid,
                        names.clone(),
                        (index + 1).min(names.len().saturating_sub(1)),
                        attach,
                    )
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::SessionContainers(
                        n,
                        name,
                        uid,
                        names,
                        index.saturating_sub(1),
                        attach,
                    )
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        let request = if attach {
                            crate::ops::Request::InteractiveAttach {
                                container: container.clone(),
                                seconds: self.options.session_seconds,
                            }
                        } else {
                            crate::ops::Request::InteractiveExec {
                                container: container.clone(),
                                argv: vec!["/bin/sh".into(), "-i".into()],
                                seconds: self.options.session_seconds,
                            }
                        };
                        self.send(Task::PrepareSession(n, name, uid, request));
                    }
                }
                _ => self.modal = Modal::SessionContainers(n, name, uid, names, index, attach),
            },
            Modal::SessionConfirm(prepared, mut text) => match code {
                KeyCode::Esc => self.status = "Session review canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if !confirmation_fits(prepared.summary(), &text, width, height) {
                        self.status = "Enlarge terminal to inspect the complete session review before approving.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else if text != prepared.confirmation() {
                        self.status = "Type the exact namespace/pod from the review.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else {
                        if prepared.is_port_forward() {
                            let generation = self.worker.forward.generation();
                            self.send(Task::RunForward(prepared, text, generation));
                        } else if prepared.is_file_copy() {
                            self.send(Task::RunSession(prepared, text));
                        } else {
                            self.pending_session = Some((prepared, text));
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::SessionConfirm(prepared, text);
                }
                KeyCode::Char(c) if c.is_ascii_graphic() && text.len() < 320 => {
                    text.push(c);
                    self.modal = Modal::SessionConfirm(prepared, text);
                }
                _ => self.modal = Modal::SessionConfirm(prepared, text),
            },
            Modal::Metrics(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('m') | KeyCode::Char('r') => {
                    self.modal = Modal::Metrics(text, offset);
                    self.request_metrics();
                }
                KeyCode::Char('M') => {
                    self.metrics_resources = true;
                    self.modal = Modal::Metrics(text, offset);
                    self.request_metrics();
                }
                _ => {
                    offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::Metrics(text, offset);
                }
            },
            Modal::Picker(label, list, mut index) => match code {
                KeyCode::Esc => {
                    if label == selections_ui::PICKER_LABEL {
                        self.restore_selection_view();
                    }
                }
                KeyCode::Delete | KeyCode::Char('d') if label == selections_ui::PICKER_LABEL => {
                    self.confirm_selection_delete(index);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    self.modal = Modal::Picker(label, list, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Picker(label, list, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(value) = list.get(index) {
                        if label == "Actions" {
                            match index {
                                2 => self.request_logs(false, false),
                                3 => self.request_logs(true, false),
                                4 => self.request_logs(false, true),
                                6 => return self.command("overview"),
                                7 => return self.scale(),
                                8 => return self.image(),
                                9 => self.prepare(Action::Restart),
                                10 => self.prepare(Action::Delete),
                                _ => {
                                    let key = match index {
                                        0 => KeyCode::Char('d'),
                                        1 => KeyCode::Char('y'),
                                        5 => KeyCode::Char('m'),
                                        11 => KeyCode::Char('J'),
                                        12 => KeyCode::Char('s'),
                                        13 => KeyCode::Char('M'),
                                        14 => KeyCode::Char('V'),
                                        _ => KeyCode::Null,
                                    };
                                    return self.key(key);
                                }
                            }
                        } else if label == "Aliases" {
                            if let Some(hotkey) = value.strip_prefix("hotkey ") {
                                if let Some(command) = hotkey
                                    .split_once(" :")
                                    .and_then(|(_, rest)| rest.split("  ").next())
                                {
                                    return self.command(command);
                                }
                            } else if let Some(resource) = value.split_whitespace().next() {
                                return self.command(resource);
                            }
                        } else if label == "Extensions" {
                            if let Some(name) = value.split_whitespace().next() {
                                self.run_extension(name);
                            }
                        } else if label == selections_ui::PICKER_LABEL {
                            if let Some(selection) = self.selection_ui.records.get(index).cloned()
                                && let Err(error) = self.open_selection(selection)
                            {
                                self.status = error;
                                self.modal = Modal::Picker(label, list, index);
                            }
                        } else if label == "Saved resources" {
                            if let Some(record) = self.saved.get(index) {
                                match crate::screendump::read(&record.id) {
                                    Ok(text) => self.modal = Modal::Detail(text, 0),
                                    Err(error) => self.status = error,
                                };
                            }
                        } else if label.starts_with("Context") {
                            if !self.busy {
                                self.push_view();
                                if label == "Context (Pod view)"
                                    && let Ok(index) = self.resolve_resource("pods")
                                {
                                    self.kind_index = index;
                                    self.desired_resource = None;
                                }
                                self.options.connection.context = Some(value.clone());
                                self.connect();
                            } else {
                                self.status =
                                    "Wait for the current request before changing context.".into();
                            }
                        } else {
                            self.push_view();
                            self.namespace = if value == "all" {
                                "*".into()
                            } else {
                                value.clone()
                            };
                            self.namespace_override = true;
                            self.desired_resource = None;
                            if let Ok(index) = self.resolve_resource("v1/pods") {
                                self.kind_index = index;
                            }
                            self.changed();
                        }
                    }
                }
                _ => self.modal = Modal::Picker(label, list, index),
            },
            Modal::Themes(list, mut index, old) => match code {
                KeyCode::Esc => self.theme = old,
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    index = index.saturating_sub(1);
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Enter => {
                    themes::use_theme(&self.theme.id)?;
                    self.status = format!("Theme saved: {}", self.theme.name);
                }
                KeyCode::Char('N') => {
                    self.modal = Modal::NewTheme(String::new(), self.theme.clone(), old);
                }
                _ => self.modal = Modal::Themes(list, index, old),
            },
            Modal::NewTheme(mut id, source, old) => match code {
                KeyCode::Esc => self.theme = old,
                KeyCode::Enter => {
                    let result = themes::create_from_theme(&id, &source).and_then(|path| {
                        themes::use_theme(&id)?;
                        Ok(path)
                    });
                    match result {
                        Ok(_) => {
                            self.theme = themes::find(&id)?;
                            self.status = format!("Custom theme created and selected: {id}");
                        }
                        Err(error) => {
                            self.status = error;
                            self.modal = Modal::NewTheme(id, source, old);
                        }
                    }
                }
                KeyCode::Backspace => {
                    id.pop();
                    self.modal = Modal::NewTheme(id, source, old);
                }
                KeyCode::Char(c) if c.is_ascii_alphanumeric() || c == '-' => {
                    if id.len() < 48 {
                        id.push(c);
                    }
                    self.modal = Modal::NewTheme(id, source, old);
                }
                _ => self.modal = Modal::NewTheme(id, source, old),
            },
            Modal::Confirm(p, mut text) => match code {
                KeyCode::Esc => self.status = "Operation canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if !confirmation_fits(&p.summary, &text, width, height) {
                        self.status="Enlarge terminal to inspect the complete confirmation before approving.".into();
                        self.modal = Modal::Confirm(p, text);
                        return Ok(false);
                    }
                    match Approval::confirm(p.target.clone(), p.action.clone(), &text) {
                        Ok(a) => self.send(Task::Execute(p, a)),
                        Err(e) => {
                            self.status = e;
                            self.modal = Modal::Confirm(p, text);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Confirm(p, text);
                }
                KeyCode::Char(c) => {
                    if c.is_ascii_graphic() && text.len() < 320 {
                        text.push(c);
                    }
                    self.modal = Modal::Confirm(p, text);
                }
                _ => self.modal = Modal::Confirm(p, text),
            },
            Modal::Scale(k, n, name, uid, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Ok(replicas) = text.parse() {
                        self.send(Task::Prepare(k, n, name, uid, Action::Scale(replicas)));
                    } else {
                        self.status = "Enter replicas 0..20".into();
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && text.len() < 2 => {
                    text.push(c);
                    self.modal = Modal::Scale(k, n, name, uid, text);
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Scale(k, n, name, uid, text);
                }
                _ => self.modal = Modal::Scale(k, n, name, uid, text),
            },
            Modal::Resources(k, n, name, uid, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if self.busy {
                        self.status = "Busy; wait for the current request before preparing the resource budget.".into();
                        self.modal = Modal::Resources(k, n, name, uid, text);
                        return Ok(false);
                    }
                    let fields: Vec<_> = text.split_whitespace().collect();
                    let result = if fields.len() == 5 {
                        let resources = crate::edit::ResourceEdit {
                            cpu_request: fields[1].into(),
                            memory_request: fields[2].into(),
                            cpu_limit: fields[3].into(),
                            memory_limit: fields[4].into(),
                        };
                        resources.validate(fields[0]).map(|_| Action::SetResources {
                            container: fields[0].into(),
                            resources,
                        })
                    } else {
                        Err("Enter five fields: container CPU_REQUEST MEMORY_REQUEST CPU_LIMIT MEMORY_LIMIT".into())
                    };
                    match result {
                        Ok(action) => self.send(Task::Prepare(k, n, name, uid, action)),
                        Err(error) => {
                            self.status = error;
                            self.modal = Modal::Resources(k, n, name, uid, text);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Resources(k, n, name, uid, text);
                }
                KeyCode::Char(c) if (c.is_ascii_graphic() || c == ' ') && text.len() < 240 => {
                    text.push(c);
                    self.modal = Modal::Resources(k, n, name, uid, text);
                }
                _ => self.modal = Modal::Resources(k, n, name, uid, text),
            },
            Modal::Help(offset) => match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {}
                _ => self.modal = Modal::Help(scroll_offset(TUI_HELP, offset, code)),
            },
            Modal::None => match code {
                KeyCode::Char('q') => return Ok(true),
                KeyCode::Esc => {
                    if !self.filter.is_empty() {
                        let uid = self.selected_uid();
                        self.filter.clear();
                        self.selected = uid
                            .and_then(|uid| {
                                self.visible().iter().position(|object| {
                                    object["metadata"]["uid"].as_str() == Some(&uid)
                                })
                            })
                            .unwrap_or(0);
                    } else if !self.selector.is_empty() {
                        self.selector.clear();
                        self.changed();
                    } else {
                        self.go_back();
                    }
                }
                KeyCode::Char('[') => self.go_back(),
                KeyCode::Char(']') => self.go_forward(),
                KeyCode::Char('-') => self.last_view(),
                KeyCode::Tab | KeyCode::BackTab => {
                    self.command_cursor = 0;
                    self.modal = Modal::Input("Command", String::new());
                }
                KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home | KeyCode::End => {
                    let page = terminal::size()
                        .map(|(w, h)| Layout::new(w, h).table_rows())
                        .unwrap_or(15)
                        .max(1);
                    let len = self.visible().len();
                    self.selected = match code {
                        KeyCode::PageDown => self
                            .selected
                            .saturating_add(page)
                            .min(len.saturating_sub(1)),
                        KeyCode::PageUp => self.selected.saturating_sub(page),
                        KeyCode::Home => 0,
                        _ => len.saturating_sub(1),
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Enter => {
                    self.drilldown();
                }
                KeyCode::Char('/') => {
                    self.modal = Modal::Filter(
                        self.filter.clone(),
                        self.filter.clone(),
                        self.selected_uid(),
                        self.selected,
                    )
                }
                KeyCode::Char(':') => {
                    self.history_index = None;
                    self.completion = None;
                    self.command_cursor = 0;
                    self.modal = Modal::Input("Command", String::new());
                }
                KeyCode::Char('J') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Get(k, n, name));
                    }
                }
                KeyCode::Char('d') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Inspect(k, n, name));
                    }
                }
                KeyCode::Char('S') => self.set_sort(Sort::Status),
                KeyCode::Char('N') => self.set_sort(Sort::Name),
                KeyCode::Char('A') => self.set_sort(Sort::Age),
                KeyCode::Char('E') => self.request_shell(),
                KeyCode::Char('n') => {
                    if let Some((_, namespace, _)) = self.selected_target() {
                        self.copy_text(&namespace)?;
                        self.status =
                            "Namespace sent to terminal clipboard (OSC52 support required).".into();
                    }
                }
                KeyCode::Char('c') => {
                    if self.kind() == "nodes" && self.options.connection.writable {
                        self.node_maintenance(crate::node_maintenance::Request::Cordon);
                        return Ok(false);
                    }
                    if let Some(object) = self.visible().get(self.selected) {
                        self.copy_text(name(object))?;
                        self.status =
                            "Name sent to terminal clipboard (OSC52 support required).".into();
                    }
                }
                KeyCode::Char('t') => {
                    if self.kind() == "cronjobs.batch" {
                        self.prepare(Action::Trigger);
                    } else {
                        self.status = "Copy files with :cp-from / :cp-to (--allow-file-copy). t triggers a selected CronJob.".into()
                    }
                }
                KeyCode::Char('b') => self.save_resource_selection()?,
                KeyCode::Char('B') => self.open_saved_selections()?,
                KeyCode::Char('?') => self.modal = Modal::Help(0),
                KeyCode::Char('D') => self.open_dashboard(vec![self.context.clone()])?,
                KeyCode::Char('u') => {
                    self.node_maintenance(crate::node_maintenance::Request::Uncordon)
                }
                KeyCode::Char('z') => self.status =
                    "Inspection is already redacted; additional k9s sanitization is unavailable."
                        .into(),
                KeyCode::Char('r') => {
                    if self.kind() == "nodes" {
                        self.node_maintenance(crate::node_maintenance::Request::Drain {
                            seconds: 120,
                        });
                        return Ok(false);
                    }
                    if crate::edit::supports_resource(&self.kind()) {
                        self.prepare(Action::Restart);
                        return Ok(false);
                    }
                    if self.watch_blocked {
                        self.options.connection.context = Some(self.context.clone());
                        self.connect();
                        self.namespace_override = true;
                    } else if self.connected {
                        self.refresh = true;
                    } else if !self.busy {
                        self.connect();
                    }
                }
                KeyCode::Char('R') => self.set_sort(Sort::Ready),
                KeyCode::Char('m') => {
                    self.metrics_resources = false;
                    self.request_metrics();
                }
                KeyCode::Char('M' | 'C') => {
                    if matches!(self.kind().as_str(), "pods" | "nodes") {
                        self.set_sort(if code == KeyCode::Char('C') {
                            Sort::Cpu
                        } else {
                            Sort::Memory
                        });
                        if !self.busy && self.last_table_metrics.is_none() {
                            self.refresh_table_metrics();
                        }
                    } else {
                        self.status="CPU/memory usage sorting applies to Pod and Node tables; :metrics shows exact samples.".into();
                    }
                }
                KeyCode::Char('w') => {
                    if let Some((_, namespace, _)) = self.selected_target() {
                        return self.command(&format!("ns {namespace}"));
                    }
                }
                KeyCode::Char('0') => return self.command("ns all"),
                KeyCode::Char(digit @ '1'..='9') => {
                    let index = usize::from(digit as u8 - b'1');
                    let recent = self.recent.namespaces(&self.context);
                    match recent.get(index).cloned() {
                        Some(namespace) => return self.command(&format!("ns {namespace}")),
                        None => {
                            self.status = if recent.is_empty() {
                                "No recent namespaces yet; :ns opens the picker. 1-9 then jump to the last nine.".into()
                            } else {
                                format!(
                                    "Recent: {}",
                                    recent
                                        .iter()
                                        .enumerate()
                                        .map(|(i, ns)| format!("{} {ns}", i + 1))
                                        .collect::<Vec<_>>()
                                        .join("  ")
                                )
                            }
                        }
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(uid) = self.selected_uid() {
                        if !self.marks.remove(&uid) {
                            self.marks.insert(uid);
                        }
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                        self.status = format!(
                            "{} marked | mutations still review one selected UID",
                            self.marks.len()
                        );
                    }
                }
                KeyCode::Char('a') => self.request_session(true),
                KeyCode::Char('y') | KeyCode::Char('v') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Yaml(k, n, name));
                    }
                }
                KeyCode::Char('o') => {
                    if self.kind() != "pods" {
                        self.status = "Show Node applies to core Pods.".into();
                        return Ok(false);
                    }
                    let node = self
                        .visible()
                        .get(self.selected)
                        .and_then(|object| object["spec"]["nodeName"].as_str())
                        .map(str::to_owned);
                    if let Some(node) = node {
                        return self.command(&format!("v1/nodes /^{node}$"));
                    }
                    self.status =
                        "Show Node requires a scheduled Pod. :overview opens multiple contexts."
                            .into();
                }
                KeyCode::Char('e') => self.yaml_editor(),
                KeyCode::Char('V') => {
                    if !self.options.connection.writable {
                        self.status = "READ ONLY: resource budgets require --write.".into();
                    } else if !crate::edit::supports_resource(&self.kind()) {
                        self.status =
                            "Resource budgets apply to supported workload containers.".into();
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.modal =
                            Modal::Resources(k, n, name, self.selected_uid(), String::new());
                    }
                }
                KeyCode::Char('l') => self.request_logs(false, true),
                KeyCode::Char('L' | 'p') => self.request_logs(true, false),
                KeyCode::Char('s') => {
                    if self.kind() == "pods" {
                        self.request_shell();
                    } else if self.kind() == "nodes" {
                        self.node_shell();
                    } else if self.kind() == "cronjobs.batch" {
                        let suspended = self
                            .visible()
                            .get(self.selected)
                            .is_some_and(|cron| cron["spec"]["suspend"] == true);
                        self.prepare(Action::Suspend(!suspended));
                    } else {
                        return self.scale();
                    }
                }
                KeyCode::Char('f') => self.modal = Modal::ForwardView,
                KeyCode::Char('F') => self.request_forward(),
                KeyCode::Char('x') => {
                    self.status =
                        "Use :xray RESOURCE for relationships or :restart for a reviewed rollout."
                            .into()
                }
                KeyCode::Delete => self.prepare(Action::Delete),
                _ => {}
            },
        }
        Ok(false)
    }
    pub(super) fn key_event(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
        if matches!(self.modal, Modal::AdviceRunning)
            && modifiers.contains(KeyModifiers::CONTROL)
            && code == KeyCode::Char('c')
        {
            if let Some(cancel) = self.advice_cancel.take() {
                let _ = cancel.send(());
            }
            self.status = "Canceling Copilot; waiting for process and connection cleanup.".into();
            return Ok(false);
        }
        if matches!(self.modal, Modal::HelmRunning(..))
            && modifiers.contains(KeyModifiers::CONTROL)
            && code == KeyCode::Char('c')
        {
            self.cancel_helm();
            return Ok(false);
        }
        if self.write_inflight {
            return self.key(code);
        }
        if matches!(self.modal, Modal::YamlEditor(..)) {
            if let Modal::YamlEditor(mut editor) = std::mem::replace(&mut self.modal, Modal::None) {
                match editor.key(crossterm::event::KeyEvent::new(code, modifiers)) {
                    Ok(crate::ui_yaml::Event::Cancel) => self.close_subview(),
                    Ok(crate::ui_yaml::Event::Review) => {
                        if self.busy {
                            self.modal = Modal::YamlEditor(editor);
                            self.status = "Wait for the current request; draft retained.".into();
                        } else {
                            self.send(Task::YamlPrepare(editor));
                        }
                    }
                    Ok(crate::ui_yaml::Event::External) => {
                        if self.options.external_editor {
                            self.pending_editor = true;
                        } else {
                            self.status = "Start KBorg with --allow-external-editor to edit drafts in $EDITOR.".into();
                        }
                        self.modal = Modal::YamlEditor(editor);
                    }
                    Ok(crate::ui_yaml::Event::Continue) => self.modal = Modal::YamlEditor(editor),
                    Err(error) => {
                        self.status = error;
                        self.modal = Modal::YamlEditor(editor);
                    }
                }
            }
            return Ok(false);
        }
        if matches!(self.modal, Modal::None)
            && let Some(hotkey) = crate::hotkeys::find(&self.hotkeys, code, modifiers)
        {
            let command = hotkey.command.clone();
            return self.command(&command);
        }
        if modifiers.contains(KeyModifiers::CONTROL) {
            if let Modal::WriteReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.view.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::AdviceReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::HelmForm(form) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    form.clear();
                }
                return Ok(false);
            }
            if let Modal::HelmReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::Helm(table) = &self.modal {
                if code == KeyCode::Char('r') {
                    let request = table.refresh();
                    self.request_helm(request, false);
                } else if code == KeyCode::Char('d') {
                    if let Some(current) = table.current() {
                        self.begin_helm_form(helm_ui::Intent::Uninstall(current));
                    }
                } else if code == KeyCode::Char('s') {
                    self.request_export();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::Follow(_)) {
                if code == KeyCode::Char('r') {
                    if self.busy {
                        self.status = "Wait for the current log request before refreshing.".into();
                        return Ok(false);
                    }
                    if let Modal::Follow(view) = std::mem::replace(&mut self.modal, Modal::None) {
                        let read = view.range;
                        self.reload_logs(view, read);
                    }
                } else if code == KeyCode::Char('s') {
                    self.request_export();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::Input("Command", _)) {
                match code {
                    KeyCode::Char('u' | 'w') => {
                        self.command_cursor = 0;
                        self.completion = None;
                        self.modal = Modal::Input("Command", String::new());
                        return Ok(false);
                    }
                    KeyCode::Char('f') => return self.key(KeyCode::Tab),
                    KeyCode::Char('e') => return self.key(KeyCode::Enter),
                    KeyCode::Char('p') => return self.key(KeyCode::Up),
                    KeyCode::Char('n') => return self.key(KeyCode::Down),
                    _ => return Ok(false),
                }
            }
            if let Modal::Input(_, text) = &mut self.modal {
                if code == KeyCode::Char('u') {
                    text.clear();
                }
                return Ok(false);
            }
            if let Modal::Editor(draft) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    draft.fields[draft.active].clear();
                }
                return Ok(false);
            }
            if let Modal::ForwardForm(_, _, _, fields, index) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    fields[*index].clear();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::None) {
                match code {
                    KeyCode::Char('a') => self.aliases(),
                    KeyCode::Char('w') => {
                        self.wide = !self.wide;
                        self.status = if self.wide {
                            "Wide columns on (Node, IP and |W view columns). Ctrl-W turns them off."
                        } else {
                            "Wide columns off."
                        }
                        .into();
                    }
                    KeyCode::Char('z') => {
                        self.faults_only = !self.faults_only;
                        self.selected = 0;
                        self.status = if self.faults_only {
                            "Errors only: unhealthy Pods, degraded workloads, NotReady Nodes, warnings. Ctrl-Z shows all."
                        } else {
                            "Showing all objects."
                        }
                        .into();
                    }
                    KeyCode::Char('d') => self.prepare(Action::Delete),
                    KeyCode::Char('k') => self.kill(),
                    KeyCode::Char('r') => {
                        if self.connected {
                            self.refresh = true;
                        } else if !self.busy {
                            self.connect();
                        }
                    }
                    KeyCode::Char('s') => {
                        let values = self.visible().into_iter().cloned().collect::<Vec<_>>();
                        match crate::screendump::save(
                            &self.context,
                            &self.namespace,
                            &self.kind(),
                            &values,
                        ) {
                            Ok(record) => {
                                self.status =
                                    format!("Saved {} | :sd opens saved resources", record.title)
                            }
                            Err(error) => self.status = error,
                        };
                    }
                    KeyCode::Char(' ') => {
                        let uids = self
                            .visible()
                            .iter()
                            .take(self.selected + 1)
                            .filter_map(|object| {
                                object["metadata"]["uid"].as_str().map(str::to_owned)
                            })
                            .collect::<Vec<_>>();
                        self.marks.extend(uids);
                        self.status = format!(
                            "{} marked | actions still target one selected UID",
                            self.marks.len()
                        );
                    }
                    KeyCode::Char('\\') => {
                        self.marks.clear();
                        self.status = "Marks cleared.".into();
                    }
                    _ => {
                        self.status =
                            "This k9s shortcut is not available here; ? lists supported actions."
                                .into()
                    }
                }
            }
            return Ok(false);
        }
        if modifiers.contains(KeyModifiers::ALT) {
            return Ok(false);
        }
        self.key(code)
    }
}
