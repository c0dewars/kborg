use crate::Result;
const WORDS: &str = "ui demo exec attach forward audit contexts overview doctor resources list get watch logs metrics scale restart delete set-image startup theme config completions copilot --demo --allow-exec --allow-attach --allow-forward --no-mouse --context --namespace --kubeconfig --config --write --readonly --allow-logs --follow --previous --yaml --json --poll --theme --no-splash --color=never --help --version";
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
