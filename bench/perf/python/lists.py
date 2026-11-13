import sys


def main(n):
    xs = []
    x = 12345
    for _ in range(n):
        x = (x * 1103515245 + 12345) % 2147483648
        xs.append(x)
    xs.sort()
    return xs[0] + xs[len(xs) // 2] + xs[-1]


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
