"""Test fixture: signals it started, then sleeps long enough to be cancelled.

Protocol v1 workflow that never completes on its own. It writes the run id to
`started.marker` inside the workspace temp directory so the test knows the
request arrived, then sleeps. Cancellation (or the timeout) must terminate it.
"""

import json
import os
import sys
import time

request = json.load(sys.stdin)
marker = os.path.join(request["workspaceTempDirectory"], "started.marker")
with open(marker, "w", encoding="utf-8") as handle:
    handle.write(request["runId"])

time.sleep(60)

print(json.dumps({"ok": True}))
