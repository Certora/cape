import "./complexity.spec";

methods {
    function addRoot(uint256) external envfree;
    function containsRoot(uint256) external returns (bool) envfree;
    function checkContainsRoot(uint256) external envfree;
    function getRoot(uint256) external returns (uint256) envfree;
    function getRootsMap(uint256) external returns (bool) envfree; 
    function getWriteHead() external returns (uint64) envfree;
    function getRootsLength() external returns (uint256) envfree;
}

use rule sanity;


// Any entry `root` in the `_roots` array must have `_rootsMap[root]`
invariant rootExistance(uint256 index)
    containsRoot(getRoot(index))
    {
        preserved {
            requireInvariant noDuplicates(getWriteHead(), index);
        }
    }


// The root at index `_writeHead` must be zero unless a root has been added for 
// all indicies in which case the root should be contained in the rootMap.
invariant writeHeadEmptyish()
    getRoot(getWriteHead()) == 0 || containsRoot(getRoot(getWriteHead()))
    {
        preserved {
            requireInvariant noDuplicates(
                getWriteHead(), 
                require_uint256(getWriteHead() + 1)
            );
            requireInvariant rootExistance(require_uint256(getWriteHead() + 1));
            requireInvariant writeHeadBound();
        }
    }

// `_writeHead` must never be greater than `_roots.length`.
invariant writeHeadBound()
    getWriteHead() <= require_uint64(getRootsLength());

// All elements of `_roots` must be unique.
invariant noDuplicates(uint256 i, uint256 j)
    i != j => getRoot(i) != getRoot(j)
    {
        preserved {
            requireInvariant rootExistance(i);
            requireInvariant rootExistance(j);
        }
    }


// Adding a new root must set `_rootsMap` of that root to true.
rule addRoot(uint256 newRoot) {
    addRoot(newRoot);
    assert containsRoot(newRoot);
}