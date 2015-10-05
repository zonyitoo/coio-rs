package main

import (
	"fmt"
	"net"
)

func main() {
	server, err := net.Listen("tcp", ":3000")
	if server == nil {
		panic(fmt.Sprintf("error in listen: %+v", err))
	}

	for {
		client, err := server.Accept()
		if err != nil {
			panic(fmt.Sprintf("error in accept: %+v", err))
		}

		go func() {
			buf := make([]byte, 1024*16)
			for {
				len, err := client.Read(buf)
				if err != nil {
					break
				}

				len, err = client.Write(buf[0:len])
				if err != nil {
					break
				}
			}
		}()
	}
}
