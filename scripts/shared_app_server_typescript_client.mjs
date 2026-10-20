// Helper for test_shared_app_server_reconnect.py. Requires a built SDK.
import { Codex } from "../sdk/typescript/dist/index.js";

const [executable, socket, cwd, mode] = process.argv.slice(2);
const codex = new Codex({
  codexPathOverride: executable,
  appServerSocket: socket,
});
const thread = codex.startThread({
  workingDirectory: cwd,
  skipGitRepoCheck: true,
});
const controller = new AbortController();
const abort = () => controller.abort();
if (mode === "abort") process.on("SIGUSR1", abort);
try {
  await thread.run("TypeScript shared backend test", {
    signal: controller.signal,
  });
  if (mode === "abort") throw new Error("Expected the aborted turn to fail");
  console.log(JSON.stringify({ threadId: thread.id, completed: true }));
} catch (error) {
  if (mode !== "abort" || !controller.signal.aborted) throw error;
  console.log(JSON.stringify({ threadId: thread.id, aborted: true }));
} finally {
  process.removeListener("SIGUSR1", abort);
}
