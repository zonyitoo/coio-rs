package main

import (
	"flag"
	"fmt"
	"log"
	"net"
	"sync"
	"sync/atomic"
	"time"
)

var (
	targetAddr  = flag.String("a", "127.0.0.1:3000", "target echo server address")
	testMsgLen  = flag.Int("l", 26, "test message length")
	testConnNum = flag.Int("c", 50, "test connection number")
	testSeconds = flag.Int("t", 10, "test duration in seconds")
	ioTimeout   = flag.Duration("d", 2*time.Second, "timeout for individual read/write operation in seconds")
)

func main() {
	flag.Parse()

	var (
		outNum uint64
		inNum  uint64
		stop   uint64
	)

	msg := make([]byte, *testMsgLen)

	go func() {
		time.Sleep(time.Second * time.Duration(*testSeconds))
		atomic.StoreUint64(&stop, 1)
	}()

	wg := new(sync.WaitGroup)

	for i := 0; i < *testConnNum; i++ {
		wg.Add(1)

		go func() {
			if conn, err := net.DialTimeout("tcp", *targetAddr, time.Minute*99999); err == nil {
				l := len(msg)
				recv := make([]byte, l)

			outer:
				for {

					conn.SetWriteDeadline(time.Now().Add(*ioTimeout))
					for rest := l; rest > 0; {
						i, err := conn.Write(msg)
						rest -= i
						if err != nil {
							log.Println(err)
							break outer
						}
					}

					atomic.AddUint64(&outNum, 1)

					if atomic.LoadUint64(&stop) == 1 {
						break outer
					}

					conn.SetReadDeadline(time.Now().Add(*ioTimeout))
					for rest := l; rest > 0; {
						i, err := conn.Read(recv)
						rest -= i
						if err != nil {
							log.Println(err)
							break outer
						}
					}

					atomic.AddUint64(&inNum, 1)

					if atomic.LoadUint64(&stop) == 1 {
						break outer
					}
				}
			} else {
				log.Println(err)
			}

			wg.Done()
		}()
	}

	wg.Wait()

	fmt.Println("Benchmarking:", *targetAddr)
	fmt.Println(*testConnNum, "clients, running", *testMsgLen, "bytes,", *testSeconds, "sec.")
	fmt.Println()
	fmt.Println("Speed:", outNum/uint64(*testSeconds), "request/sec,", inNum/uint64(*testSeconds), "response/sec")
	fmt.Println("Requests:", outNum)
	fmt.Println("Responses:", inNum)
}
