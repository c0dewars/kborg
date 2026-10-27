class Kborg < Formula
  desc "Fast Kubernetes terminal UI with multi-cluster dashboard and reviewed changes"
  homepage "https://github.com/c0dewars/kborg"
  url "https://github.com/c0dewars/kborg/archive/refs/tags/v0.5.0-rc.4.tar.gz"
  sha256 "cd2381cab6c9538c226b21ef13f72e778091398403ec83b4afc2c17084c1e3d0"
  license "Apache-2.0"
  head "https://github.com/c0dewars/kborg.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    generate_completions_from_executable(bin/"kborg", "completions")
  end

  test do
    assert_match "KBorg #{version}", shell_output("#{bin}/kborg --version")
    # Offline demo data: no cluster or kubeconfig is needed.
    assert_match "api-7d4f9", shell_output("#{bin}/kborg --demo list pods")
  end
end
