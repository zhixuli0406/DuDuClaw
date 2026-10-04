# Release distributor technical brief

`scripts/release.sh` generates the distributor technical brief after the Rust
checks succeed and before creating the release commit or tags. It calls:

```bash
python3 scripts/build-distributor-brief.py --version 1.69.1 --skip-missing
```

The source belongs to the separate private `commercial` repository. The default
source directory is `commercial/marketing/distributor/technology-brief-v1.67-src`;
that historical directory name is stable. The output is a standalone HTML file:
`commercial/marketing/distributor/duduclaw-technology-brief-v1.69.1.html`.
Versioned files from earlier releases remain available. The public release commit
does not stage, commit, upload, or publish the private document.

The filename and page title use the requested release version. Feature claims,
review dates, and historical version references remain as written in the source.
When the requested version differs from the source's product version, a visible
notice identifies both versions and says that the feature descriptions still need
review. Generating an edition does not certify its claims for a newer release.
Review and update the private source before distributing the document.

An absent private source directory is reported as skipped with `--skip-missing`.
An existing but incomplete source directory or a failing builder aborts the
release before commit and tag; the version bump stays uncommitted for inspection.
After correcting the source, generate the document again and resolve the pending
bump before rerunning the full release script, which requires a clean public tree.
`--dry-run` only reports the intended output and never invokes the builder.

For an isolated preview, pass `--source-dir` and `--output-dir`. Identical existing
output is accepted. Different existing output is refused unless `--force` is
explicitly passed to the renderer; the release script never passes `--force`.
The builder runs in a temporary directory so previews do not modify the private
source or its fragment copies.

Validate the renderer without running a release:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*distributor*.py'
bash -n scripts/release.sh
```
