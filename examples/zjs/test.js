// Simple test for zjs functionality
print("Hello from zjs!");

// Test basic math
let result = 2 + 3;
print("2 + 3 =", result);

// Test std module
if (typeof std !== "undefined") {
  print("std module is available");
  print("std.in:", typeof std.in);
  print("std.out:", typeof std.out);
} else {
  print("std module not available");
}

// Test os module
if (typeof os !== "undefined") {
  print("os module is available");
  print("os.now():", os.now());
} else {
  print("os module not available");
}

// Test basic array operations
let arr = [1, 2, 3, 4, 5];
print("Array length:", arr.length);
print("First element:", arr[0]);

print("Test completed!");
