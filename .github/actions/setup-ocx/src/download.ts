import * as core from "@actions/core";
import * as tc from "@actions/tool-cache";
import * as crypto from "crypto";
import * as fs from "fs";
import { getArchiveName, getDownloadUrl, getTarget } from "./constants";

export interface DownloadResult {
  binDir: string;
  version: string;
}

export async function downloadOcx(version: string, token: string): Promise<DownloadResult> {
  const { target, isWindows } = getTarget();
  const archiveName = getArchiveName(target, isWindows);

  // Check tool cache first
  const cached = tc.find("ocx", version, process.arch);
  if (cached) {
    core.info(`Found cached OCX ${version} at ${cached}`);
    const binDir = findBinDir(cached);
    return { binDir, version };
  }

  // Download archive
  const archiveUrl = getDownloadUrl(version, archiveName);
  core.info(`Downloading OCX ${version} from ${archiveUrl}`);

  const archivePath = await tc.downloadTool(archiveUrl, undefined, token ? `token ${token}` : undefined);

  // Download and verify checksum
  const checksumUrl = getDownloadUrl(version, `${archiveName}.sha256`);
  core.info(`Verifying checksum from ${checksumUrl}`);

  const checksumPath = await tc.downloadTool(checksumUrl, undefined, token ? `token ${token}` : undefined);
  const checksumContent = fs.readFileSync(checksumPath, "utf8").trim();
  // Format: "<hash>  <filename>" or just "<hash>"
  const expectedHash = checksumContent.split(/\s+/)[0];

  const fileBuffer = fs.readFileSync(archivePath);
  const actualHash = crypto.createHash("sha256").update(fileBuffer).digest("hex");

  if (actualHash !== expectedHash) {
    throw new Error(`SHA256 mismatch for ${archiveName}:\n  expected: ${expectedHash}\n  actual:   ${actualHash}`);
  }
  core.info("Checksum verified");

  // Extract archive
  let extractedDir: string;
  if (isWindows) {
    extractedDir = await tc.extractZip(archivePath);
  } else {
    const flags = archiveName.endsWith(".tar.gz") ? "xz" : "xJ";
    extractedDir = await tc.extractTar(archivePath, undefined, flags);
  }

  // Cache the extracted directory
  const cachedDir = await tc.cacheDir(extractedDir, "ocx", version, process.arch);

  const binDir = findBinDir(cachedDir);
  return { binDir, version };
}

/**
 * Finds the directory containing the ocx binary within an extracted archive.
 * cargo-dist archives have a top-level directory named like the archive.
 */
function findBinDir(dir: string): string {
  // Check if ocx binary is directly in the directory
  const entries = fs.readdirSync(dir);

  // cargo-dist puts the binary inside a subdirectory named after the archive
  // e.g., ocx-x86_64-unknown-linux-gnu/ocx
  for (const entry of entries) {
    const subPath = `${dir}/${entry}`;
    if (fs.statSync(subPath).isDirectory()) {
      const subEntries = fs.readdirSync(subPath);
      if (subEntries.some((e) => e === "ocx" || e === "ocx.exe")) {
        return subPath;
      }
    }
    if (entry === "ocx" || entry === "ocx.exe") {
      return dir;
    }
  }

  return dir;
}
