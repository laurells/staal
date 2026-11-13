package main

import (
	"fmt"
	"os"
	"strconv"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	var s int64
	for i := int64(0); i < n; i++ {
		s += (i * i) % 7
	}
	fmt.Println(s)
}
