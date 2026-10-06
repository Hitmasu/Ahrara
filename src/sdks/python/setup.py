"""Bundle the native core in a platform wheel; never download at runtime."""

from setuptools import Distribution, setup
from wheel.bdist_wheel import bdist_wheel


class NativeDistribution(Distribution):
    def has_ext_modules(self):
        return True


class NativeWheel(bdist_wheel):
    def get_tag(self):
        _, _, platform = super().get_tag()
        return "py3", "none", platform


setup(
    distclass=NativeDistribution,
    cmdclass={"bdist_wheel": NativeWheel},
)
