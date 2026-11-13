package main

import (
	"fmt"
	"os"
	"strconv"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	counts := map[int64]int64{0: 0}
	for i := int64(0); i < n; i++ {
		counts[(i*7919)%1000]++
	}
	var s int64
	for k, v := range counts {
		s += k * v
	}
	fmt.Println(s)
}
