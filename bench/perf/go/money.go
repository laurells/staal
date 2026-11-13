package main

// Exact money as integer minor units (cents), as Go code keeps it.

import (
	"fmt"
	"os"
	"strconv"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	var price, total int64 = 37, 0
	for i := int64(0); i < n; i++ {
		total += price * (i%10 + 1)
	}
	fmt.Printf("\"%d.%02d USD\"\n", total/100, total%100)
}
