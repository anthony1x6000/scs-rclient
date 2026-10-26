# Changelog

## Extension Whitelist / Blacklist

- **Filter by File Type**: Two new buttons in the console toolbar — "Whitelist extension" and "Blacklist extension" — open an editor where you type a comma-separated list (`.html,.txt`; the dot is optional and matching is case-insensitive). A whitelist transfers only the listed file types; a blacklist skips them. Files without an extension (e.g. `LICENSE`) never pass a whitelist and never match a blacklist.
- **Per-Dir or Global**: Each editor has "Confirm for this dir" (saves a list for the currently selected subdirectory only) and "Confirm for all dirs" (saves the default for every subdirectory). Confirming an empty box clears that scope. A per-dir list replaces the global one for that folder, and a whitelist beats a blacklist in the same scope; the panel shows both scopes plus which mode is actually in effect.
- **Applies Everywhere, Both Sides**: The filter gates every action (upload, download, previews, backup, compare, list, overwrite) on the local walk and the remote scan alike, so filtered files are never transferred in either direction, and the remote listing cache is bypassed while a filter is active. Bulk runs resolve each ticked folder's own list, so they stay compatible with multi-folder selection.
- **Tests**: Rust unit tests cover normalization and verdicts, live Copyparty e2e tests prove whitelist/blacklist put/get behavior against the simulated D2L endpoint, and a Vitest suite pins CSV parsing and the per-dir/global resolution rules.

## Bulk Subfolder Actions

- **Multi-Select Subfolders**: The subdirectory dropdown now shows a checkbox on every row. Ticking two or more folders switches the whole action grid into bulk mode — every button (Upload, Download, previews, Compare, List, Backup, Overwrite) relabels with the folder count (e.g. "Upload · 7 folders") and runs the action on each ticked folder sequentially, in list order. Clicking a row's name still single-selects that folder and clears the ticks.
- **Quick Add at the Bottom of the Dropdown**: A new input field at the bottom of the open dropdown adds subfolders — press Enter and the folder is appended to the list (or found if it already exists) and pre-ticked for bulk selection, and the input clears so the next one can be typed immediately. The top field is now filter-only.
- **Bulk Run Semantics**: Each folder gets a `=== [2/7] folder ===` header in the console log; a failing folder is logged and the run continues, ending with a summary line ("Bulk finished: 5 ok, 2 failed (css, img)."). Cancel stops the current folder's operation and skips the rest, which the summary reports as skipped. Overwrite keeps its double-press confirm, worded with the folder count.
- **Persistent Selection**: The ticked folders (and window title, which shows "scs-rclient: 7 folders" in bulk mode) survive app restarts, stored alongside the existing subdirectory settings.
- **Frontend Unit Tests**: The repo now has a Vitest suite (`pnpm test:unit`) for the bulk selection/runner logic, wired into CI as the "Frontend Unit Tests (Vitest)" check.

## Live Thread Changes

- **Settings Apply Mid-Operation**: Changing the Settings "Threads" value while a scan or upload is running now resizes its worker pool immediately instead of waiting for the next operation. Pools dispatch up to the live limit and re-check it as each worker claims work, so raising the value adds workers right away and lowering it stops new work from being dispatched (requests already in flight finish normally).

## Timeout Resilience

- **Retried Transient Requests**: Every WebDAV request (PROPFIND, MKCOL, PUT, GET, DELETE) now retries up to `REQUEST_MAX_ATTEMPTS` (3) times with a growing delay when it fails with a transient transport error — a timeout, connection reset, or truncated body. Permanent failures (bad credentials, 403, local disk errors) are not retried.
- **Timeouts No Longer Abort the Whole Action**: A request that still fails after every retry skips only its own item. `put`/`sync` continue uploading the remaining files, `get` removes the partial download and moves on, and the recursive scan skips the unlisted subfolder — warning loudly and listing every skipped directory in the final summary — instead of failing the entire operation. Only the root listing remains fatal, since there is nothing to traverse without it.
- **Verbose Retry Diagnostics**: Each retry prints the operation, attempt number, elapsed time, whether the error was a timeout/connect/body failure, and the full reqwest source chain, plus when a request recovers or gives up.

## Concurrent Uploads

- **Parallel PUT/Sync Uploads**: `put` and `sync` now upload files through a worker pool sized by the Settings "Threads" value (the same concurrency setting already used for remote scanning), instead of uploading files one at a time. The pool mirrors the scan design (atomic work cursor, mpsc log funneling, panic recovery) and serializes remote-cache updates behind a mutex because `update_remote_cache_item` performs a read-modify-write of the cache file. Per-file error semantics: MKCOL/open/HTTP-status failures are logged and skipped; a transport failure is retried (see Timeout Resilience above) and skipped once its retries are exhausted.

## WebDAV Security Hardening

- **Symlink Traversal Prevention**: `collect_local_files_with_mtime` skips symlinks and enforces canonical path containment checks under the root source directory to prevent arbitrary file disclosure.
- **Path Traversal Protection**: WebDAV downloads and item path resolutions sanitize and reject relative path sequences (`..`, `.`, leading slashes, backslashes). Download destinations enforce canonical containment under the target directory before writing.
- **SSRF and Protocol Validation**: Added `validate_webdav_url` blocking private RFC 1918 IPs, loopback, link-local / cloud metadata (e.g. 169.254.169.254), and carrier-grade NAT in production, while requiring HTTPS.
- **Streamed I/O & File Size Limits**: Added configurable maximum file size limit (`MAX_WEBDAV_FILE_SIZE_BYTES`, default 500 MB). Uploads and downloads stream without buffering complete file contents into RAM.
- **Safe Remote Parent Directory Creation**: `ensure_remote_parent_dirs` validates target URLs and safely handles `MKCOL` HTTP statuses without ignoring unexpected failures.
- **WebdavItem Builder API**: Added `WebdavItem::new(href, is_dir, size)` and `.with_mtime(...)` builder methods and `Default` implementation for backwards compatibility.
- **File Time Preservation Diagnostics**: Preserving remote modification timestamps on downloaded files now logs diagnostic warnings on failure rather than ignoring errors silently.
- **Deterministic Integration Tests**: E2E tests use `FileTimes` to adjust timestamps explicitly rather than relying on brittle sleep intervals in CI.
- **Local Mount Containment**: `run_webdav_action` strictly enforces `join_contained` for `subdir` under the mount directory and rejects traversal errors instead of falling back to raw path join.
