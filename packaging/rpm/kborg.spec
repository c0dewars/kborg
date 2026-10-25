# RPM for KBorg, repackaged from the release archive that CI already built and
# smoke-tested (scripts/Package-Release.ps1), so no Rust toolchain or crate
# download is needed at rpmbuild time. Build on the target architecture:
#
#   mkdir -p ~/rpmbuild/SOURCES
#   cp kborg-0.5.0-rc.1-x86_64-unknown-linux-gnu.tar.gz ~/rpmbuild/SOURCES/
#   rpmbuild -bb --define 'upstream_version 0.5.0-rc.1' packaging/rpm/kborg.spec
#
# See docs/OPERATIONS.md ("Install from a .deb or .rpm").

%{!?upstream_version: %global upstream_version 0.5.0-rc.1}
# x86_64 / aarch64 are exactly the Rust target CPU names.
%global rust_target %{_target_cpu}-unknown-linux-gnu
# The release binary is already stripped; there is nothing to split out.
%global debug_package %{nil}
# /usr/share/doc/kborg on every rpm distribution (some default to name-version).
%global _docdir_fmt %{name}

Name:           kborg
# 0.5.0-rc.1 -> 0.5.0~rc.1 so pre-releases sort before the final release.
Version:        %{lua: local v = string.gsub(rpm.expand("%{upstream_version}"), "-", "~", 1); print(v)}
Release:        1%{?dist}
Summary:        Kubernetes operations, under your control
License:        Apache-2.0
URL:            https://github.com/c0dewars/kborg
Source0:        %{name}-%{upstream_version}-%{rust_target}.tar.gz
ExclusiveArch:  x86_64 aarch64

%description
KBorg is a fast Kubernetes terminal UI with k9s-compatible keys, a
multi-cluster dashboard and a read-only-by-default safety model: every
change gets a server dry run, a readable diff and typed confirmation.

%prep
%setup -q -n %{name}-%{upstream_version}-%{rust_target}
test "$(./kborg --version)" = "KBorg %{upstream_version}"

%build
./kborg completions bash > kborg.bash
./kborg completions zsh > _kborg
./kborg completions fish > kborg.fish

%install
install -Dpm 0755 kborg %{buildroot}%{_bindir}/kborg
install -Dpm 0644 kborg.bash %{buildroot}%{_datadir}/bash-completion/completions/kborg
install -Dpm 0644 _kborg %{buildroot}%{_datadir}/zsh/site-functions/_kborg
install -Dpm 0644 kborg.fish %{buildroot}%{_datadir}/fish/vendor_completions.d/kborg.fish

%check
./kborg --demo list pods --json > /dev/null

%files
# Kept together under /usr/share/doc/kborg, matching the .deb.
%doc LICENSE THIRD_PARTY_NOTICES.md
%{_bindir}/kborg
%{_datadir}/bash-completion/completions/kborg
%{_datadir}/zsh/site-functions/_kborg
%{_datadir}/fish/vendor_completions.d/kborg.fish

%changelog
* Sun Oct 25 2026 KBorg maintainers - 0.5.0~rc.1-1
- Initial RPM packaging from the release archive.
