<!-- aos mission template: `fix <what is broken>`. Plain text you can edit;
     {goal} is replaced with what you typed. -->
# fix
Fix something that is broken.

## brief
The operator reports this problem: {goal}

1. Reproduce it first and record how (a command, a test, or steps).
2. Find the cause. Say where it is (file and line) and why it happens.
3. Fix the cause, not the symptom, with the smallest change.
4. Show the reproduction now passes, and that the project's tests still pass.
5. Have the reviewer check the fix before you submit the goal.

## acceptance
- The reproduction failed before the fix and passes after it, with the command shown.
- The cause is named with a file and line.
- Existing tests still pass, or the result says which fail and why.
