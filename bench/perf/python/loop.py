import sys


def main(n):
    s = 0
    for i in range(n):
        s += (i * i) % 7
    return s


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
