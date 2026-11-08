#!/usr/bin/env python3
"""Build the unsigned macOS gateway bundle using only the standard library."""
import argparse
import plistlib
import re
import shutil
from pathlib import Path


def build_bundle(exe, name, version, out, icon=None):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', version):
        raise ValueError('version must be X.Y.Z')
    if name not in ('duduclaw', 'duduclaw-pro'):
        raise ValueError('unsupported executable name')
    if not exe.is_file() or (icon is not None and not icon.is_file()):
        raise ValueError('executable or icon is missing')
    app = out / 'DuDuClaw Gateway.app'
    # Refuse to reuse a potentially signed or stale bundle.
    app.mkdir(parents=True, exist_ok=False)
    contents = app / 'Contents'
    (contents / 'MacOS').mkdir(parents=True)
    (contents / 'Resources').mkdir()
    binary = contents / 'MacOS' / name
    shutil.copy2(exe, binary)
    binary.chmod(0o755)
    info = dict(
        CFBundleIdentifier='ai.duduclaw.gateway',
        CFBundleName='DuDuClaw Gateway',
        CFBundleDisplayName='DuDuClaw Gateway',
        CFBundleExecutable=name,
        CFBundlePackageType='APPL',
        CFBundleShortVersionString=version,
        CFBundleVersion=version,
        CFBundleInfoDictionaryVersion='6.0',
        LSMinimumSystemVersion='11.0', LSUIElement=True,
        NSHumanReadableCopyright='Copyright © DuDu Digital. All rights reserved.',
        NSAppleEventsUsageDescription='DuDuClaw uses Apple Events to automate apps for tasks you approve.',
        NSDesktopFolderUsageDescription='DuDuClaw accesses Desktop files for tasks you approve.',
        NSDocumentsFolderUsageDescription='DuDuClaw accesses Documents files for tasks you approve.',
        NSDownloadsFolderUsageDescription='DuDuClaw accesses Downloads files for tasks you approve.',
        NSRemovableVolumesUsageDescription='DuDuClaw accesses removable drives for tasks you approve.',
        NSNetworkVolumesUsageDescription='DuDuClaw accesses network drives for tasks you approve.',
        NSLocalNetworkUsageDescription='DuDuClaw connects to local devices and services for tasks you approve.',
    )
    if icon is not None:
        shutil.copy2(icon, contents / 'Resources/AppIcon.icns')
        info['CFBundleIconFile'] = 'AppIcon.icns'
    with (contents / 'Info.plist').open('wb') as stream:
        plistlib.dump(info, stream)
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, required=True)
    parser.add_argument('--name', choices=('duduclaw', 'duduclaw-pro'), required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--icon', type=Path)
    args = parser.parse_args()
    try:
        print(build_bundle(args.exe, args.name, args.version, args.out, args.icon))
    except (ValueError, OSError) as error:
        parser.exit(1, f'Bundle build failed: {error}\n')


if __name__ == '__main__':
    main()
