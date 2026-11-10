Fix the saved Node.js implementation of one first-layer virtual function. module_fix_input supplies its file, source, contract, current arguments, error location, output and completed writes; skill_instructions supplies relevant library instructions.

Find the cause and edit only the supplied module file with edit_file. Each edit is compiled; correct any diagnostic. Preserve the contract and other allowed inputs. Keep code light: delegate complex understanding, judgment and generation to focused @virtual helpers; ordinary code calls libraries, prepares inputs and checks results. Consult real API contracts when needed.

Verify the relevant behavior and reply with one short sentence describing the fix. The host saves the new version and restarts this call, so preserve completed work and never replay uncertain external writes. If the contract cannot be met, return {"$task_error":{"message":"concrete cause"}}.
