"""RapidRAW export workflow protocol v1 review fixture (postImage phase).

RapidRAW export workflows are TRUSTED LOCAL CODE and run with your user
permissions. Never install a workflow from a source you do not trust.

The host launches this file with a Python interpreter, writes exactly one
WorkflowRequest JSON document to stdin, and reads exactly one WorkflowResponse
JSON document from stdout. Anything written to stderr is captured as
diagnostic text only. Uses only the Python standard library.
"""

import json
import os
import sys

PROTOCOL_VERSION = 1


def failure(message):
    return {
        "ok": False,
        "message": message,
        "warnings": [],
        "producedArtifactPaths": [],
    }


def main():
    raw = sys.stdin.read()
    try:
        request = json.loads(raw)
    except json.JSONDecodeError as error:
        print(json.dumps(failure(f"invalid request JSON: {error}")))
        return 0

    if request.get("protocolVersion") != PROTOCOL_VERSION:
        print(
            json.dumps(
                failure(
                    "unsupported workflow protocolVersion {!r}; expected {}".format(
                        request.get("protocolVersion"), PROTOCOL_VERSION
                    )
                )
            )
        )
        return 0

    if request.get("phase") != "postImage":
        print(json.dumps(failure("expected phase postImage")))
        return 0

    workspace = request.get("workspaceTempDirectory") or "."
    receipt = {
        "runId": request.get("runId"),
        "workflowId": request.get("workflowId"),
        "sourcePath": request.get("sourcePath"),
        "exportedPath": request.get("exportedPath"),
    }
    # producedArtifactPaths are relative to the workspace temp directory.
    receipt_name = "receipt.json"
    with open(os.path.join(workspace, receipt_name), "w", encoding="utf-8") as handle:
        json.dump(receipt, handle)

    response = {
        "ok": True,
        "message": "wrote receipt for {}".format(request.get("sourcePath")),
        "warnings": [],
        "producedArtifactPaths": [receipt_name],
    }
    print(json.dumps(response))
    return 0


if __name__ == "__main__":
    sys.exit(main())
