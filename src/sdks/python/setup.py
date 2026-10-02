"""Bundle the native core in a platform wheel; never download at runtime."""

import os
from pathlib import Path
import shutil

from setuptools import Distribution, setup
from setuptools.command.build_py import build_py
from wheel.bdist_wheel import bdist_wheel


class NativeBuild(build_py):
    def run(self):
        super().run()
        source = os.environ.get("AHRARA_NATIVE_LIBRARY")
        if not source or not Path(source).is_absolute() or not Path(source).is_file():
            raise RuntimeError(
                "Build the native core and set AHRARA_NATIVE_LIBRARY to its absolute path before packaging."
            )

        target = Path(self.build_lib) / "ahrara" / "native"
        target.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target / Path(source).name)


class NativeDistribution(Distribution):
    def has_ext_modules(self):
        return True


class NativeWheel(bdist_wheel):
    def get_tag(self):
        _, _, platform = super().get_tag()
        return "py3", "none", platform


setup(
    distclass=NativeDistribution,
    cmdclass={"build_py": NativeBuild, "bdist_wheel": NativeWheel},
)
