import time
import debugpy
debugpy.listen(5678)
debugpy.wait_for_client()
for i in range(5):
    val = i * 2
    print(val)
print('Done')
