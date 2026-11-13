package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"unicode/utf8"
)

func main() {
	n, _ := strconv.Atoi(os.Args[1])
	var b strings.Builder
	for i := 0; i < n/10; i++ {
		b.WriteString("abcdéfghij")
	}
	t := b.String()
	var s int64
	for j := 0; j < 1000; j++ {
		s += int64(utf8.RuneCountInString(t))
		if strings.Contains(t, strconv.Itoa(j)) {
			s++
		}
	}
	fmt.Println(s)
}
