import * as core from "@actions/core";
import { downloadOcx } from "./download";

async function run(): Promise<void> {
  try {
    const version = core.getInput("version", { required: true });
    const token = core.getInput("github-token");

    const { binDir, version: installedVersion } = await downloadOcx(version, token);

    core.addPath(binDir);
    core.setOutput("version", installedVersion);
    core.setOutput("ocx-path", binDir);

    core.info(`OCX ${installedVersion} is ready (${binDir})`);
  } catch (error) {
    if (error instanceof Error) {
      core.setFailed(error.message);
    } else {
      core.setFailed("An unexpected error occurred");
    }
  }
}

run();
