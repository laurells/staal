# staal

The `staalc` compiler and runtime, installed with pip, and a Python interface to it.
No Rust toolchain is needed.

    pip install staal
    staalc check program.staal
    staalc run program.staal --config run.json

From Python, through the C interface:

    import staal
    lib = staal.Library()          # the library installed with this package
    print(lib.check("program.staal"))

`staalc build` also needs a C compiler (`cc`) to link executables.
