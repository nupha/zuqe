// D6: explicit interrupt must return immediately with its exit code, exercising
// the ExitStatus::Interrupted path (the loop must not shadow it with UNDEFINED).
import * as std from "std";
std.exit(7);
