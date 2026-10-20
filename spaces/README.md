# Space Map Content

`content/` is the complete logical map catalog embedded in kernel builds. It
may be replaced as a unit before compiling a release. Keep map art and tilemap
assets in the client; the kernel reads only each version's
`space-package.json`.

## Update Order

- For art-only changes, release the client only.
- For logical map changes:
  1. Release the client first and keep both old and new logical versions.
  2. Update `content/`, point `current_version` at the new version, and release
     the kernel.
  3. After every node has upgraded, remove the old version from both the client
     and `content/`.

Before release, compare each
`content/**/space-package.json` with its corresponding client file:

```sh
shasum -a 256 spaces/content/**/space-package.json
shasum -a 256 /path/to/wattetheria-client/web/public/spaces/**/space-package.json
```
