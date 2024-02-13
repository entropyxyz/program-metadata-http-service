# program-metadata-web-service

HTTP JSON API which hosts a database of program metadata of open source programs which can be verified that the source code corresponds to the on-chain binary.

## Requirements:

Docker is required in order to build programs deterministically and git is required to be able to clone program repos.

## Usage:

Start the http server with:
`cargo run`

The following http usage examples use the http client [httpie](https://httpie.io).

## Adding a program

There are two ways to add a program's metadata and get the hash of the compiled program in the response.

### Adding a program from a public git repo

Give the git repo location which is passed directly to `git clone`, like so:

```bash
echo -n "https://github.com/ameba23/program-always-fails.git" | http post localhost:3000/build-git
```

### Adding a program's source code directly using `tar`.

You can pipe a program's source code to the service `tar`:

```bash
cd some_example_program
tar cvf - . | http post localhost:3000/build-tar
```

Be aware this may fail if you accidentally include the `./target` directory, and the http request becomes too big.

You can tell tar to exclude stuff like this:

```bash
tar --exclude='./target' --exclude='./.git' -cvf - . | http post localhost:3000/build-tar
```

## Getting program metadata

You can get a list of all program hashes as a JSON encoded array of hex strings:

```bash
http localhost:3000/programs
```

Example output:
```json 
[
    "64871473c40795324d86d6cb0a42c0a2b546fefe02785d8f6f0124ac2b2200e9",
    "7d6ae77343476f9e585e23f81731fe2d287a3d9cc003cbd73235c2a2634e2ebe",
    "a947e55b58659b5abaed2c710b9a6741fc728c81fd5b44201953745372597be5"
]
```

You can get JSON metadata about a particular program using the hex encoded hash of its binary:

```bash
http localhost:3000/program/a947e55b58659b5abaed2c710b9a6741fc728c81fd5b44201953745372597be5
```

Example output:
```json
{
    "authors": [
        "peg <ameba23@systemli.org>"
    ],
    "categories": [],
    "default_run": null,
    "dependencies": [
        {
            "features": [],
            "kind": "normal",
            "name": "entropy-programs-core",
            "optional": false,
            "path": null,
            "registry": null,
            "rename": null,
            "req": "*",
            "source": "git+https://github.com/entropyxyz/programs.git?tag=v0.8.0",
            "target": null,
            "uses_default_features": true
        }
    ],
    "description": null,
    "documentation": null,
    "edition": "2021",
    "features": {},
    "homepage": null,
    "id": "program-always-fails 0.1.0 (path+file:///tmp/turnip/tf5ffe-1)",
    "keywords": [],
    "license": "Unlicense",
    "license_file": null,
    "links": null,
    "manifest_path": "/tmp/turnip/tf5ffe-1/Cargo.toml",
    "metadata": {
        "component": {
            "dependencies": {},
            "package": "entropy:program-always-fails"
        },
        "entropy-program": {
            "docker-image": "peg997/build-entropy-programs:version0.1"
        }
    },
    "name": "program-always-fails",
    "publish": null,
    "readme": "README.md",
    "repository": "https://github.com/ameba23/program-always-fails",
    "rust_version": null,
    "source": null,
    "targets": [
        {
            "crate_types": [
                "cdylib"
            ],
            "doc": true,
            "doctest": false,
            "edition": "2021",
            "kind": [
                "cdylib"
            ],
            "name": "program-always-fails",
            "required-features": [],
            "src_path": "/tmp/turnip/tf5ffe-1/src/lib.rs",
            "test": true
        }
    ],
    "version": "0.1.0"
}
```
