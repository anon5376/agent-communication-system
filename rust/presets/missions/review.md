<!-- aos mission template: `review <what to review>`. Plain text you can edit;
     {goal} is replaced with what you typed. -->
# review
Review code or a change and report problems. Changes no code.

## brief
Review this: {goal}

Have the reviewer read the code and run the project's tests or build where
they exist. Report problems ranked by how much they matter, each with the
file and line, what is wrong, and what would fix it. Do not edit the code.

## acceptance
- Findings are ranked, most serious first, each with a file and line.
- Every command run is listed with its real outcome.
- If nothing serious was found, the result says so and what was checked.
