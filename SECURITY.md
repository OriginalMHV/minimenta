# Security Policy

## Supported Versions

Only the latest release gets security fixes. minimenta is at version 0.x, so each fix ships in a new 0.x release. Update to the latest release before you report a problem.

## Reporting a Vulnerability

**Do not open a public issue for a security problem.**

Use a private report on GitHub:

<https://github.com/OriginalMHV/minimenta/security/advisories/new>

Include this information:

1. What happens, and what an attacker can do.
2. The steps to reproduce the problem.
3. The output of `minimenta --version`, the operating system, and the file system.
4. A fix, if you have one.

What to expect:

- You get an answer within 48 hours.
- For a confirmed problem, you get a fix or a plan within 7 days.
- The report becomes public after the fix is released.

## What minimenta Does with Your Files and Privileges

minimenta is a local program. It does not use the network and does not send data anywhere. It runs with the rights of the user who starts it, and it does not lower its rights after it starts.

These actions need care:

1. **It deletes and moves files.** The `d` key moves the selected items to the Trash. The `D` key deletes them for good. Both keys ask for confirmation first. On macOS, minimenta asks Finder to move the items, and it uses the file manager API when Finder is not available. On Linux and Windows, it uses the `trash` crate.
2. **It acts with the rights of the user.** If you start minimenta as root or as an administrator, `d` and `D` can move or delete any file that this account can change. Start minimenta as a normal user when you want to remove files.
3. **It reads the NTFS master file table as administrator (Windows).** minimenta opens the NTFS volume for reading. It reads the master file table in large blocks. It never writes to the volume. The `--no-mft` option turns this off. Without administrator rights, minimenta lists directories the normal way.
4. **It keeps a cache (macOS).** minimenta stores the names and sizes of the folders that you scanned in `~/Library/Caches/minimenta`. It sets no special permissions on these files. minimenta does not use the cache when it runs as root. The `--no-cache` option turns the cache off.

## What Counts as a Security Issue

- A path or link bug that makes minimenta move or delete an item that the user did not select.
- A crafted file name, symlink, junction, or file system structure that makes minimenta write, move, delete, or read outside the selected items.
- A damaged or hostile NTFS volume, cache file, or directory tree that causes memory unsafety in the `unsafe` code, in the NTFS parser, or in the macOS system calls.
- A dependency with a known vulnerability that minimenta reaches. CI checks the dependencies with `cargo deny check`.

## What Does Not Count

- A problem that needs an attacker who already controls your account. minimenta is a local tool.
- A deletion that you selected and confirmed.
- Wrong sizes, slow scans, and crashes that cause no memory unsafety and no data loss. Use a normal issue for these.
- Feature requests.
