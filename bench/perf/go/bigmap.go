package main

import (
	"fmt"
	"os"
	"strconv"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	m := map[int64]int64{0: 0}
	for i := int64(1); i < n; i++ {
		m[(i*7919)%1000003] = i
	}
	var s int64
	for j := int64(0); j < n; j++ {
		if v, ok := m[(j*7919)%1000003]; ok {
			s += v
		} else {
			s--
		}
	}
	fmt.Println(s)
}
