import sys


def main(n):
    t = "".join("abcdéfghij" for _ in range(n // 10))
    s = 0
    for j in range(1000):
        s += len(t)
        if str(j) in t:
            s += 1
    return s


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
