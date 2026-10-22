using Alias = int;
struct Record { int *field; };
int execute(int parameter) {
    int local = parameter;
    Record record;
    record.field = &local;
    return local;
}
// ghost local
const char *message = "ghost local";
