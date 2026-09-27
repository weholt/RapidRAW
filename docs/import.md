# Preview-first import

On desktop, select image files or a source folder, optionally include subfolders, choose a
destination, and arrange metadata or literal blocks into separate folder and filename patterns.
Every destination is resolved by Rust and shown before Import is enabled. Changing sources,
destination, patterns, operation, or collision handling creates a new immutable plan ID.

## Tokens and missing values

Patterns support original filename/stem/extension, sequence, capture year/month/day/hour/minute,
make, model, lens model, ISO, artist, copyright, description, keywords, rating, color label,
headline, and location. Existing templates such as
`{YYYY}/{MM}/{original_filename}_{sequence}` remain supported. The original extension is retained.

Missing metadata can use an empty value, a token fallback, or block the plan. The safe default is
a fallback and filename `originalStem`. See
[the metadata decision](decisions/import-metadata.md) for exact precedence and XMP/IPTC coverage.

## Collisions and path safety

The default collision action adds a numeric suffix. Skip and blocking-error policies are also
available; import never silently overwrites. Absolute paths, traversal, symlink escapes, Windows
device names, illegal characters, trailing dots/spaces, duplicate batch destinations, and
overlong components are rejected or deterministically sanitized. Rust rechecks containment and
collisions immediately before each write; the frontend never supplies trusted per-row paths.

## Copy, move, and recovery

Copy is the default. Each output is written to a uniquely named temporary file in its destination
directory, flushed, synced, checked by byte count and SHA-256, and atomically finalized. Selected
`.rrdata`, `.rrexif`, and `.xmp` sidecars follow the renamed image.

Move first completes and verifies the image and all sidecars. Only then are sources sent to the
desktop system trash. A trash failure retains the source and is reported; RapidRAW never falls
back to permanent deletion. Failures are isolated per source group, and cancellation stops before
the next destructive boundary. The detailed result remains available for copying after completion.

Android uses the platform content picker and a copy-only path with original names. Move and source
deletion are intentionally unavailable because Storage Access Framework URIs cannot honor desktop
filesystem semantics.
