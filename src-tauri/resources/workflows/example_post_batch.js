// RapidRAW example export workflow (postBatch phase).
//
// RapidRAW export workflows are TRUSTED LOCAL CODE and run with your full
// user permissions: they can read, modify, and delete any file you can, and
// they can access the network. Never install a workflow from a source you do
// not trust.
//
// A standalone `.js` file is a complete workflow. No manifest and no
// third-party packages are required; defaults are derived from the file name
// and extension (see docs/export-workflows.md). This file additionally has an
// adjacent `example_post_batch.js.rapidraw.json` sidecar that overrides the
// display name, description, order, and timeout - copy it to any
// `<script>.rapidraw.json` to do the same for your own workflows.
//
// RapidRAW launches this file with the detected node executable, writes
// exactly one workflow request JSON document to stdin, and reads exactly one
// response JSON document from stdout. Anything written to stderr is captured
// as diagnostic text only. This example validates the request, writes a
// harmless sidecar receipt into the per-run workspace temp directory, and
// reports the receipt as a produced artifact. It uses only Node core modules,
// is deterministic, and works offline.

const fs = require('fs');
const path = require('path');

const PROTOCOL_VERSION = 1;

function respond(response) {
  process.stdout.write(JSON.stringify(response));
}

function failure(message) {
  return {
    ok: false,
    message: message,
    warnings: [],
    producedArtifactPaths: [],
  };
}

function readStdin() {
  return new Promise((resolve) => {
    let data = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (chunk) => {
      data += chunk;
    });
    process.stdin.on('end', () => resolve(data));
  });
}

async function main() {
  let request;
  try {
    request = JSON.parse(await readStdin());
  } catch (error) {
    respond(failure(`invalid request JSON: ${error.message}`));
    return;
  }

  if (request.protocolVersion !== PROTOCOL_VERSION) {
    respond(failure(`unsupported workflow protocolVersion ${request.protocolVersion}; expected ${PROTOCOL_VERSION}`));
    return;
  }

  if (request.phase !== 'postBatch') {
    respond(failure('expected phase postBatch'));
    return;
  }

  const workspace = request.workspaceTempDirectory || '.';
  const exportedItems = request.exportedItems || [];
  const selectedItems = request.selectedItems || [];

  // producedArtifactPaths are relative to the workspace temp directory.
  const receiptName = 'receipt.json';
  fs.writeFileSync(
    path.join(workspace, receiptName),
    JSON.stringify(
      {
        runId: request.runId,
        workflowId: request.workflowId,
        exportedCount: exportedItems.length,
        selectedCount: selectedItems.length,
      },
      null,
      2,
    ),
  );

  respond({
    ok: true,
    message: `wrote receipt for ${exportedItems.length} exported of ${selectedItems.length} selected items`,
    warnings: [],
    producedArtifactPaths: [receiptName],
  });
}

main();
