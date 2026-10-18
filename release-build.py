import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

# Configuration
APP_NAME = "chromatic-ez-firmware"
RELEASE_DIR = Path("target/release")
OUTPUT_ZIP_DIR = Path("dist")


def run_command(cmd, shell=False):
    """Utility function to run a command and raise on failure."""
    print(f"Executing: {' '.join(cmd) if isinstance(cmd, list) else cmd}")
    subprocess.run(cmd, check=True, shell=shell)

def exe_name(stem):
    return f"{APP_NAME}-{stem}.exe" if sys.platform == "win32" else f"{APP_NAME}-{stem}"


def main():
    root_dir = Path.cwd()

    # 1. Build for release
    print("Building project for release...")
    run_command(["cargo", "build", "--release"])

    # 2. Build third-party-notices.html using cargo about
    print("Generating third-party-notices.html...")
    notice_file = root_dir / "third-party-notices.html"
    run_command(
        ["cargo", "about", "generate", "about.hbs", "-o", str(notice_file)]
    )

    exe_paths = [
        RELEASE_DIR / exe_name("cli"),
        RELEASE_DIR / exe_name("gui"),
    ]

    # 3. If on Windows, sign the executable using signtool
    if sys.platform == "win32":
        print("Signing executable...")

        # Retrieve Certificate Thumbprint or Container Name from environment, or set defaults
        # EV certificates with SSL.com typically use eSigner CKA_ID or an installed certificate thumbprint
        cert_thumbprint = os.getenv("SIGNTOOL_THUMBPRINT")

        signtool_cmd = [
            "signtool",
            "sign",
            "/fd",
            "SHA256",
            "/tr",
            "http://ts.ssl.com",
            "/td",
            "SHA256",
        ]

        if cert_thumbprint:
            signtool_cmd.extend(["/sha1", cert_thumbprint])
        else:
            signtool_cmd.extend(["/a"])

        signtool_cmd.extend([str(p) for p in exe_paths])

        run_command(signtool_cmd)

    # 4. Create ZIP package
    OUTPUT_ZIP_DIR.mkdir(exist_ok=True)
    zip_path = OUTPUT_ZIP_DIR / f"{APP_NAME}.zip"

    files_to_zip = [
        (root_dir / "COPYING", "COPYING"),
        (notice_file, "third-party-notices.html"),
    ]
    files_to_zip.extend([(p, p.name) for p in exe_paths])

    print(f"Creating ZIP archive at {zip_path}...")
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zipf:
        for src, arcname in files_to_zip:
            if not src.exists():
                raise FileNotFoundError(f"Required file not found: {src}")
            zipf.write(src, arcname)
            print(f"  Added: {arcname}")

    print("Build and packaging complete successfully.")


if __name__ == "__main__":
    main()
