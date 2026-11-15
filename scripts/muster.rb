# Filled in and pushed to the tap by `make cask`.
cask "muster" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/@REPO@/releases/download/v#{version}/Muster-#{version}-macos.zip"
  name "Muster"
  desc "Workspace for agentic coding: run, review and merge coding agents"
  homepage "https://github.com/@REPO@"

  depends_on macos: ">= :big_sur"

  app "Muster.app"
  binary "#{appdir}/Muster.app/Contents/MacOS/muster"

  # Signed ad hoc rather than notarized, so Gatekeeper would refuse to open it.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Muster.app"]
  end

  zap trash: [
    "~/.config/muster",
    "~/.muster",
  ]
end
