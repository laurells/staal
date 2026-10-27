# The wheel holds a compiled staalc and its library for one platform, but no Python
# extension: it is tagged for that platform and for any Python 3 ("py3-none-<platform>").
# STAAL_WHEEL_PLATFORM names the platform instead of the building Python's own, which on
# macOS is the macOS it was built for, not the one the binaries need
# (scripts/wheel.sh sets it from MACOSX_DEPLOYMENT_TARGET).
import os

from setuptools import setup
from setuptools.command.bdist_wheel import bdist_wheel
from setuptools.dist import Distribution


class BinaryDistribution(Distribution):
    # Compiled files, so the wheel installs them where platform files go.
    def has_ext_modules(self):
        return True


class PlatformWheel(bdist_wheel):
    def finalize_options(self):
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self):
        _, _, platform = super().get_tag()
        return "py3", "none", os.environ.get("STAAL_WHEEL_PLATFORM") or platform


setup(distclass=BinaryDistribution, cmdclass={"bdist_wheel": PlatformWheel})
