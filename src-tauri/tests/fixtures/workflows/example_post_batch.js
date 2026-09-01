// RapidRAW export workflow protocol v1 review fixture (postBatch phase).
//
// RapidRAW export workflows are TRUSTED LOCAL CODE and run with your user
// permissions. Never install a workflow from a source you do not trust.
//
// The host launches this file with the node executable, writes exactly one
// WorkflowRequest JSON document to stdin, and reads exactly one
// WorkflowResponse JSON document from stdout. Anything written to stderr is
// captured as diagnostic text only. Uses only Node core modules.

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
  const exportedCount = (request.exportedItems || []).length;
  const selectedCount = (request.selectedItems || []).length;

  // producedArtifactPaths are relative to the workspace temp directory.
  const receiptName = 'receipt.json';
  fs.writeFileSync(
    path.join(workspace, receiptName),
    JSON.stringify({
      runId: request.runId,
      workflowId: request.workflowId,
      exportedCount,
      selectedCount,
    }),
  );

  respond({
    ok: true,
    message: `reviewed ${exportedCount} exported of ${selectedCount} selected items`,
    warnings: [],
    producedArtifactPaths: [receiptName],
  });
}

main();
