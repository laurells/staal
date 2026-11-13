import sys


def main(n):
    m = {0: 0}
    for i in range(1, n):
        m[(i * 7919) % 1000003] = i
    s = 0
    for j in range(n):
        v = m.get((j * 7919) % 1000003)
        s += v if v is not None else -1
    return s


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
