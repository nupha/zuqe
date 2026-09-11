var j;
var i;
var str;
var res;

var whitespaceChars = [
  0x0009, 0x000a, 0x000b, 0x000c, 0x000d, 0x0020, 0x00a0, 0x1680, 0x2000,
  0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009,
  0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
];

for (j = 0x0000; j < 0x10000; j++) {
  if (j === 0x180e) {
    continue;
  } // Skip 0x180E, current test in a separate file
  if (j === 0xfeff) {
    continue;
  } // Ignore BOM
  str = String.fromCharCode(j);
  res = str.replace(/\S+/g, "test262");
  // if (whitespaceChars.indexOf(j) >= 0) {
  //   assert.sameValue(res, str, "WhiteSpace character, charCode: " + j);
  // } else {
  //   assert.sameValue(
  //     res,
  //     "test262",
  //     "Non WhiteSpace character, charCode: " + j
  //   );
  // }
}
