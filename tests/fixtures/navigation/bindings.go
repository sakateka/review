package main
func execute(parameter int) int {
    local := parameter
    left, right := local, local
    return local + left + right
}
// ghost local
var message = "ghost local"
