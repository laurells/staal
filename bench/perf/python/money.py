# Exact money as decimal.Decimal, as Python code keeps it.
import sys
from decimal import Decimal


def main(n):
    price = Decimal("0.37")
    total = Decimal("0.00")
    for i in range(n):
        total += price * (i % 10 + 1)
    return f'"{total:.2f} USD"'


if __name__ == "__main__":
    print(main(int(sys.argv[1])))
