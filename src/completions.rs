use crate::Result;
const WORDS: &str = "ui dashboard demo view yaml-template yaml-edit cordon uncordon drain --allow-yaml-edit --allow-force-kill --contexts --dashboard --allow-external-editor --allow-secret-reveal --allow-node-maintenance helm history detail values install upgrade rollback uninstall crds crds-upgrade crds-delete crds-migrate test pulses xray workloads can dir auth-helper extensions exec attach forward audit contexts overview doctor resources list get watch logs metrics scale restart delete kill set-image set-resources startup theme config completions copilot preview diagnose preflight --demo --interactive --session-seconds --log-seconds --tail --since-seconds --head --selector --metrics-resources --allow-auth-helper --auth-helper-policy --allow-extensions --allow-tools --tool --tool-arg --allow-exec --allow-attach --allow-forward --no-mouse --context --namespace --kubeconfig --config --write --readonly --allow-logs --allow-helm --allow-helm-write --allow-helm-hooks --allow-helm-crds --helm-tool --helm-values --helm-timeout --helm-registry-ca --helm-registry-auth --helm-keyring --helm-sigstore-policy --copilot-policy --all-containers --helm-storage --follow --previous --yaml --json --poll --theme --no-splash --color=never --help --version";
/// Static completions never inspect a cluster or interpolate resource data.
pub fn script(shell: &str) -> Result<String> {
    Ok(match shell {
        "bash" => format!(
            "_kborg() {{ COMPREPLY=( $(compgen -W '{WORDS}' -- \"${{COMP_WORDS[COMP_CWORD]}}\") ); }}\ncomplete -F _kborg kborg\n"
        ),
        "zsh" => format!("#compdef kborg\n_arguments '*:command:({WORDS})'\n"),
        "fish" => WORDS
            .split_whitespace()
            .map(|word| {
                if let Some(flag) = word.strip_prefix("--") {
                    format!("complete -c kborg -l {flag}\n")
                } else {
                    format!("complete -c kborg -f -a {word}\n")
                }
            })
            .collect(),
        "powershell" => format!(
            "Register-ArgumentCompleter -Native -CommandName kborg -ScriptBlock {{ param($wordToComplete, $commandAst, $cursorPosition)\n  '{WORDS}'.Split(' ') | Where-Object {{ $_.StartsWith($wordToComplete, [System.StringComparison]::OrdinalIgnoreCase) }} | ForEach-Object {{ [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }}\n}}\n"
        ),
        _ => return Err("unsupported completion shell; use bash, zsh, fish, powershell".into()),
    })
}
