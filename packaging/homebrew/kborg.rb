class Kborg < Formula
  desc "Fast Kubernetes terminal UI with multi-cluster dashboard and reviewed changes"
  homepage "https://github.com/c0dewars/kborg"
  url "https://github.com/c0dewars/kborg/archive/refs/tags/v0.9.0.tar.gz"
  sha256 "fb296e11873ee708fea7045a69a424178b469e6f01ca2394f24dab10dbd8dc70"
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
