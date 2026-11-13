package main

import (
	"fmt"
	"os"
	"slices"
	"strconv"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	var xs []int64
	x := int64(12345)
	for i := int64(0); i < n; i++ {
		x = (x*1103515245 + 12345) % 2147483648
		xs = append(xs, x)
	}
	slices.Sort(xs)
	l := len(xs)
	fmt.Println(xs[0] + xs[l/2] + xs[l-1])
}
