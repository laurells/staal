import sys


def main(n):
    counts = {0: 0}
    for i in range(n):
        k = (i * 7919) % 1000
        counts[k] = counts.get(k, 0) + 1
    return sum(k * v for k, v in counts.items())


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
