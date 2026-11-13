# Text built from parts joined once, as Python code builds a long text.
import sys


def main(n):
    parts = []
    for i in range(n):
        parts.append(str(i))
        parts.append(",")
    return len("".join(parts))


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
