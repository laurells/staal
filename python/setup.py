# The wheel holds a compiled staalc and its library for one platform, but no Python
# extension: it is tagged for that platform and for any Python 3 ("py3-none-<platform>").
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
        return "py3", "none", platform


setup(distclass=BinaryDistribution, cmdclass={"bdist_wheel": PlatformWheel})
