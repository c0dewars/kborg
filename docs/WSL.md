# WSL2 development and validation

K8Borg runs as a native Linux executable in WSL2. The Windows executable is a separate build; running it through Windows interop does not count as Linux validation.

The reference environment is Ubuntu26.04.1 on WSL2, kernel6.18.33.2-microsoft-standard-WSL2, on a Windows host with32GiB RAM and16logical CPUs. WSL exposes approximately15.6GiB RAM and4GiB swap. Rust1.99.0 was installed inside Ubuntu from the official rustup-init distribution after checking its published SHA256; Ubuntu build-essential, pkg-config and ca-certificates supply the Linux linker and certificates. These are development tools, not runtime dependencies of the shipped executable.

## Docker Desktop integration

Reuse the existing Docker Desktop daemon. A second Docker Engine installation in Ubuntu creates a separate daemon and complicates cluster and image selection.

Docker Desktop's normal Ubuntu CLI integration is currently disabled on this host. Its existing daemon was independently verified from the distro as root through the Desktop-owned shared proxy socket: Docker29.8.2,16CPUs,16319672320bytes memory. This diagnostic access is not a recommended runtime setup. The application does not use the Docker socket and needs no Docker group/root privileges.

For normal Docker commands in Ubuntu, enable Ubuntu-26.04 under Docker Desktop **Settings > Resources > WSL Integration**, then apply the change. [Docker's official WSL documentation](https://docs.docker.com/desktop/features/wsl/) describes this supported setup. No Docker daemon, unauthenticated TCP listener, socket permission change, or second engine was installed for these tests.

The existing Kubernetes endpoint `https://127.0.0.1:64170` is reachable from this Ubuntu distro. Native Linux K8Borg completed live TLS-verified reads and the isolated mutation acceptance suite using the existing embedded Docker Desktop CA and short-lived fixture credentials. The port is host-specific; regenerate fixtures if it changes. TLS verification remains enabled.

## Reproduce

From PowerShell7 in the project root:

```powershell
# Offline tests, Clippy, Linux build and resource measurements:
.\scripts\Test-WSL.ps1

# After Test-DockerDesktop.ps1 -KeepFixtures succeeds:
.\scripts\Test-WSL.ps1 -FixtureDirectory .\.local\<run-id>
```

The live suite intentionally mutates only the validated `k8borg-tests-<run-id>` namespace. It tests scale, restart and a disposable service deletion. Because deletion is part of acceptance, provision a fresh fixture run before rerunning this suite. A previously retained fixture can be used for read-only performance measurements without recreating the service. Credentials expire after approximately one hour; do not commit or export them.

The launcher resolves Windows paths with wslpath and passes them as arguments. Cargo artifacts live in the distro's `$HOME/.cache/k8borg-target` to avoid sharing Windows build artifacts or paying NTFS compilation overhead. The Linux binary and checksum are copied to ignored `dist/linux-x86_64/`. Credentials are read from the owned `.local` fixture directory and never included in the build output.

Native Linux tests, Clippy and live acceptance passed on2026-10-09. See PERFORMANCE.md for measured resource usage and the limits of the100-context test. This WSL result does not prove behavior on every Linux distribution or provide macOS validation.
