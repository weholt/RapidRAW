/**
 * Test fixture: signals it started, then sleeps long enough to be cancelled.
 *
 * Protocol v1 workflow that never completes on its own. It writes the run id
 * to `started.marker` inside the workspace temp directory so the test knows
 * the request arrived, then sleeps. Cancellation (or the timeout) must
 * terminate it.
 */
const fs = require('fs');
const path = require('path');

let raw = '';
process.stdin.on('data', (chunk) => {
  raw += chunk;
});
process.stdin.on('end', () => {
  const request = JSON.parse(raw);
  const marker = path.join(request.workspaceTempDirectory, 'started.marker');
  fs.writeFileSync(marker, request.runId);
  setTimeout(() => {
    process.stdout.write(JSON.stringify({ ok: true }));
  }, 60000);
});
