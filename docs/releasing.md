# Publishing a release

One `vX.Y.Z` tag publishes the CLI, the SDKs and the browser app. The relay is
deployed separately. The variables, secrets and environments each
[workflow](../.github/workflows/) needs are referenced there by name.

1. Run the relevant [checks](../CONTRIBUTING.md#checks) on `main`, including
   `make update-test` and `make sdk-test`. The version needs no commit: the tag sets
   it (`make version VERSION=X.Y.Z` does the same locally).
2. Create the tag: push it (`git tag vX.Y.Z && git push origin vX.Y.Z`) or publish
   a GitHub release with a new `vX.Y.Z` tag, which stays a draft until its binaries
   are attached. The
   [Client release workflow](../.github/workflows/client-release.yml) sets that
   version in every manifest and publishes the CLI to npm, as the
   `ghcr.io/hitmasu/ahrara` image and, with its signed `stable.json`, to the GitHub
   Release; publishes the SDKs to npm, PyPI, NuGet and Maven Central; tags the Go SDK
   and k6 extension modules; and commits the Homebrew formula and the version to
   `main`. The [Browser app workflow](../.github/workflows/web-app.yml) deploys the
   browser app from the same tag; run it by hand to publish `main`. A failed
   publishing job can be re-run and skips what is already published.
3. Check an installation through Homebrew, npm and the repository plugin:

   ```sh
   brew tap hitmasu/ahrara https://github.com/Hitmasu/Ahrara.git
   brew install hitmasu/ahrara/ahrara
   brew test hitmasu/ahrara/ahrara
   npm install -g ahrara && ahrara --version
   ```
