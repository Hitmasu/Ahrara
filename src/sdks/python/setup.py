"""Bundle the native core in a platform wheel; never download at runtime."""

import os

from setuptools import Distribution, setup
from setuptools.command.bdist_wheel import bdist_wheel


class NativeDistribution(Distribution):
    def has_ext_modules(self):
        return True


class NativeWheel(bdist_wheel):
    def get_tag(self):
        _, _, platform = super().get_tag()
        # Release builds name the platform of the bundled library, which is
        # cross-compiled, instead of the build machine's.
        return "py3", "none", os.environ.get("AHRARA_WHEEL_PLATFORM") or platform


setup(
    distclass=NativeDistribution,
    cmdclass={"bdist_wheel": NativeWheel},
)
