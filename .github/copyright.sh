#!/bin/bash

# If there are new files with headers that can't match the conditions here,
# then the files can be ignored by an additional glob argument via the -g flag.
# For example:
#   -g "!src/special_file.rs"
#   -g "!src/special_directory"

# The tables layerstack_schemagen generates derive from OpenUSD's schema
# definitions and carry OpenUSD's license; they are checked below.
generated="layerstack_schemas/src/generated"

# Check all the standard Rust source files
output=$(rg "^// Copyright (19|20)[\d]{2} (.+ and )?the LayerStack Authors( and .+)?$\n^// SPDX-License-Identifier: Apache-2\.0 OR MIT$\n\n" --files-without-match --multiline -g "*.rs" -g "!$generated/*.rs" .)

if [ -n "$output" ]; then
	echo -e "The following files lack the correct copyright header:\n"
	echo $output
	echo -e "\n\nPlease add the following header:\n"
	echo "// Copyright $(date +%Y) the LayerStack Authors"
	echo "// SPDX-License-Identifier: Apache-2.0 OR MIT"
	echo -e "\n... rest of the file ...\n"
	exit 1
fi

# Check the generated tables: OpenUSD's copyright, then ours, under the
# Tomorrow Open Source Technology License 1.0.
output=$(rg "^// Copyright 2016 Pixar$\n^// Copyright (19|20)[\d]{2} the LayerStack Authors$\n^// SPDX-License-Identifier: LicenseRef-TOST-1\.0$\n" --files-without-match --multiline -g "*.rs" "$generated")

if [ -n "$output" ]; then
	echo -e "The following generated files lack the generated header:\n"
	echo $output
	echo -e "\n\nRegenerate them with layerstack_schemagen.\n"
	exit 1
fi

echo "All files have correct copyright headers."
exit 0
