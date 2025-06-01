import package_version, requests

version = package_version.get_version()
if requests.get(f"https://crates.io/api/v1/crates/anglerkit/{version}").status_code != 404:
    print(f"error: anglerkit {version} already exists on crates.io")
    exit(1)