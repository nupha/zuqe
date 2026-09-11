

function pad(str, n) {
  str += "";
  while (str.length < n)
    str += " ";
  return str;
}

function pad_left(str, n) {
  str += "";
  while (str.length < n)
    str = " " + str;
  return str;
}

function pad_center(str, n) {
  str += "";
  while (str.length < n) {
    if ((n - str.length) & 1)
      str = str + " ";
    else
      str = " " + str;
  }
  return str;
}


function log_line() {
  var i, n, s, a;
  s = "";
  for (i = 0, n = arguments.length; i < n; i++) {
    if (i > 0)
      s += " ";
    a = arguments[i];
    if (typeof a == "number") {
      total[i] += a;
      a = toPrec(a, precs[i]);
      s += pad_left(a, widths[i]);
    } else {
      s += pad_left(a, widths[i]);
    }
  }
  console.log(s);
}

var heads = ["TEST", "N", "TIME (ns)", "REF (ns)", "SCORE (%)"];
var widths = [22, 10, 9, 9, 9];
var precs = [0, 0, 2, 2, 2];
var total = [0, 0, 0, 0, 0];

log_line.apply(null, heads);
