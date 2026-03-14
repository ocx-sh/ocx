export const REPO_OWNER = "ocx-sh";
export const REPO_NAME = "ocx";

/** Maps Node.js process.platform to Rust target OS. */
const PLATFORM_MAP: Record<string, string> = {
  linux: "unknown-linux-gnu",
  darwin: "apple-darwin",
  win32: "pc-windows-msvc",
};

/** Maps Node.js process.arch to Rust target arch prefix. */
const ARCH_MAP: Record<string, string> = {
  x64: "x86_64",
  arm64: "aarch64",
};

export function getTarget(): { target: string; isWindows: boolean } {
  const platform = PLATFORM_MAP[process.platform];
  const arch = ARCH_MAP[process.arch];

  if (!platform) {
    throw new Error(`Unsupported platform: ${process.platform}`);
  }
  if (!arch) {
    throw new Error(`Unsupported architecture: ${process.arch}`);
  }

  const target = `${arch}-${platform}`;
  const isWindows = process.platform === "win32";
  return { target, isWindows };
}

export function getArchiveName(target: string, isWindows: boolean): string {
  const ext = isWindows ? ".zip" : ".tar.xz";
  return `ocx-${target}${ext}`;
}

export function getDownloadUrl(version: string, filename: string): string {
  return `https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/download/v${version}/${filename}`;
}
