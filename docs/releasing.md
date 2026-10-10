# Publishing a release

One `vX.Y.Z` tag publishes the CLI, the SDKs and the browser app. The relay is
deployed separately. The variables, secrets and environments each
[workflow](../.github/workflows/) needs are referenced there by name.

1. Set the new version in `src/cli/Cargo.toml` and in the other manifests and
   lockfiles. `make client-test` fails naming any manifest whose version differs.
   Run the relevant [checks](../CONTRIBUTING.md#checks), including
   `make update-test` and `make sdk-test`.
2. Push the tag (`git tag vX.Y.Z && git push origin vX.Y.Z`) and approve the
   `release` environment. The
   [Client release workflow](../.github/workflows/client-release.yml) publishes the
   CLI to npm and, with its signed `stable.json`, to the GitHub Release; commits the
   Homebrew formula to `main`; publishes the SDKs to npm, PyPI, NuGet and Maven
   Central; and tags the Go SDK and k6 extension modules. The
   [Browser app workflow](../.github/workflows/web-app.yml) deploys the browser app
   from the same tag; run it by hand to publish `main`. A failed publishing job can
   be re-run and skips what is already published.
3. Check an installation through Homebrew, npm and the repository plugin:

   ```sh
   brew tap hitmasu/ahrara https://github.com/Hitmasu/Ahrara.git
   brew install hitmasu/ahrara/ahrara
   brew test hitmasu/ahrara/ahrara
   npm install -g ahrara && ahrara --version
   ```
