# Import metadata catalog

Import planning reads metadata without modifying the source. Values are limited to 500 UTF-8
bytes and sidecars to 2 MiB.

Precedence, highest first:

1. `<image>.rrdata` RapidRAW metadata and edits
2. `<image>.rrexif` legacy RapidRAW metadata
3. `<stem>.xmp` or `<image>.xmp`
4. Embedded EXIF or RAW metadata through `exif_processing`
5. Synthetic filename, stem, extension, and sequence values

Supported normalized fields are capture year/month/day/hour/minute, camera make/model, lens
model, ISO, artist/creator, copyright, description/caption, keywords, rating, color label,
headline, and location. XMP coverage is intentionally limited to those fields; arbitrary IPTC
or XMP properties are not promised.

A catalog entry records its raw value, normalized value, source, and explicit missing state.
Missing values never become the strings `undefined` or `null`.
