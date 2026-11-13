package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

func main() {
	n, _ := strconv.ParseInt(os.Args[1], 10, 64)
	var t strings.Builder
	for i := int64(0); i < n; i++ {
		t.WriteString(strconv.FormatInt(i, 10))
		t.WriteByte(',')
	}
	fmt.Println(t.Len())
}
