"""RapidRAW example export workflow (postImage phase).

RapidRAW export workflows are TRUSTED LOCAL CODE and run with your full user
permissions: they can read, modify, and delete any file you can, and they can
access the network. Never install a workflow from a source you do not trust.

A standalone ``.py`` file is a complete workflow. No manifest and no
third-party packages are required; defaults are derived from the file name and
extension (see docs/export-workflows.md). Direct files default to the
``postBatch`` phase, so this postImage example has a minimal adjacent sidecar
``example_post_image.py.rapidraw.json`` containing only
``{"phase": "postImage"}`` - every other property keeps its derived default.
``example_post_batch.js.rapidraw.json`` next to the Node example shows a full
metadata sidecar.

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

    if request.get("phase") != "postImage":
        respond(failure("expected phase postImage"))
        return 0

    workspace = request.get("workspaceTempDirectory") or "."
    receipt = {
        "runId": request.get("runId"),
        "workflowId": request.get("workflowId"),
        "phase": request.get("phase"),
        "sourcePath": request.get("sourcePath"),
        "exportedPath": request.get("exportedPath"),
    }
    # producedArtifactPaths are relative to the workspace temp directory.
    receipt_name = "receipt.json"
    with open(os.path.join(workspace, receipt_name), "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")

    respond(
        {
            "ok": True,
            "message": "wrote receipt for {}".format(request.get("sourcePath")),
            "warnings": [],
            "producedArtifactPaths": [receipt_name],
        }
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
