// VM-P2 A/B probe: monomorphic property read / read-write hot loops.
// Timing with Date.now(); each kernel runs both orders across two passes to
// damp machine drift (memo vm_todo §3 requirement: paired + alternating).
var N = 4000000;

function prop_read(n) {
    var obj, sum, j;
    obj = { a: 1, b: 2, c: 3, d: 4 };
    sum = 0;
    for (j = 0; j < n; j++) {
        sum += obj.a;
        sum += obj.b;
        sum += obj.c;
        sum += obj.d;
    }
    if (sum !== n * 10) throw "bad sum " + sum;
    return n * 4;
}

function prop_rw(n) {
    var obj, sum, j;
    obj = { a: 1, b: 2, c: 3, d: 4 };
    sum = 0;
    for (j = 0; j < n; j++) {
        obj.a = obj.b;
        obj.c = obj.d;
        sum += obj.a + obj.c;
    }
    if (obj.a !== 2 || obj.c !== 4) throw "bad write";
    return n * 2;
}

function run(name, f, n) {
    f(1000); // warm
    var t = Date.now();
    f(n);
    var dt = Date.now() - t;
    console.log(name + ": " + dt + "ms");
    return dt;
}

// alternating order across two rounds
run("prop-read #1", prop_read, N);
run("prop-rw  #1", prop_rw, N);
run("prop-rw  #2", prop_rw, N);
run("prop-read #2", prop_read, N);
