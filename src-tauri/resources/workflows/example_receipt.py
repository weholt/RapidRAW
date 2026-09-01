"""RapidRAW example export workflow (direct-file defaults).

RapidRAW export workflows are TRUSTED LOCAL CODE and run with your full user
permissions: they can read, modify, and delete any file you can, and they can
access the network. Never install a workflow from a source you do not trust.

This file is the smallest possible workflow: a standalone ``.py`` file with no
sidecar and no third-party packages. Every property is derived from the file
name and extension - id ``example-receipt``, display name "example receipt",
phase ``postBatch`` (one invocation after the whole batch settles), default
order, timeout, and error policy. See docs/export-workflows.md and the
sidecars next to ``example_post_image.py`` and ``example_post_batch.js`` for
optional metadata overrides.

RapidRAW launches this file with the detected Python interpreter, writes
exactly one workflow request JSON document to stdin, and reads exactly one
response JSON document from stdout. Anything written to stderr is captured as
diagnostic text only. This example validates the request, writes a harmless
sidecar receipt into the per-run workspace temp directory, and reports the
receipt as a produced artifact. It uses only the Python standard library, is
deterministic, and works offline.
"""

import json
import os
import sys

PROTOCOL_VERSION = 1


def failure(message):
    return {"ok": False, "message": message, "warnings": [], "producedArtifactPaths": []}


def respond(response):
    print(json.dumps(response))


def main():
    try:
        request = json.loads(sys.stdin.read())
    except json.JSONDecodeError as error:
        respond(failure("invalid request JSON: {}".format(error)))
        return 0

    if request.get("protocolVersion") != PROTOCOL_VERSION:
        respond(
            failure(
                "unsupported workflow protocolVersion {!r}; expected {}".format(
                    request.get("protocolVersion"), PROTOCOL_VERSION
                )
            )
        )
        return 0

    if request.get("phase") != "postBatch":
        respond(failure("expected phase postBatch"))
        return 0

    workspace = request.get("workspaceTempDirectory") or "."
    exported = request.get("exportedItems") or []
    selected = request.get("selectedItems") or []
    receipt = {
        "runId": request.get("runId"),
        "workflowId": request.get("workflowId"),
        "exportedCount": len(exported),
        "selectedCount": len(selected),
    }
    # producedArtifactPaths are relative to the workspace temp directory.
    receipt_name = "receipt.json"
    with open(os.path.join(workspace, receipt_name), "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")

    respond(
        {
            "ok": True,
            "message": "wrote receipt for {} exported of {} selected items".format(
                len(exported), len(selected)
            ),
            "warnings": [],
            "producedArtifactPaths": [receipt_name],
        }
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
