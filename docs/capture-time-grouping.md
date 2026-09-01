# Capture-session grouping

Capture-session grouping adds visual section headers to the grid and list library views. Enable it in
**Library → View Options → Capture Sessions**, then choose an adjacent-gap threshold from 1 to 120 minutes.
The default is disabled with a 15-minute threshold.

Images are ordered chronologically within each source folder. A new session starts only when the time
between an image and the immediately preceding image is greater than the threshold; a gap exactly equal
to the threshold remains in the same session. Recursive browsing never combines images from different
folders. Disabling grouping restores the sort selection that was active before grouping because grouping
does not overwrite that setting.

Capture time is resolved in this order:

1. EXIF `DateTimeOriginal`, including `OffsetTimeOriginal` and `SubSecTimeOriginal` when present.
2. EXIF `CreateDate`, including its digitized offset and subseconds when present.
3. The file modified timestamp.

Headers show a warning indicator when the final fallback is used. RAW/JPEG same-stem collapsing, filters,
and search are applied before sessions are formed. Session headers are display-only: they are not
selectable and never enter image navigation, rating, metadata, export, or culling arrays. Culling view
intentionally remains an uninterrupted image-only list.
